//! The daemon's answers to [`WorkspaceMethod`] requests, and who may send them.
use std::sync::Arc;

use anyhow::Result;
use tokio::net::UnixStream;

use super::super::{
    answer::{Answer, OperationGuard},
    scope::{Caller, Scoped, denied},
    workspace::{self, Manager},
};
use crate::protocol::{self, Body, ConfigTarget, ErrorCode, Response, WorkspaceMethod};

pub(super) async fn handle(
    manager: &Arc<Manager>,
    method: WorkspaceMethod,
    caller: Option<&Caller>,
) -> Result<Answer> {
    Ok(Answer::Body(match method {
        WorkspaceMethod::WatchWorkspaceEvents { since, follow } => {
            let manager = manager.clone();
            return Ok(Answer::connection(move |stream, request_id, guard| {
                watch_workspace_events(stream, request_id, manager, since, follow, guard)
            }));
        }
        WorkspaceMethod::OpenBranch {
            path,
            repository,
            branch,
            git_profile,
            base,
            chat,
        } => Body::OpenedWorkspace(
            manager
                .open_branch_for_chat(
                    repository,
                    &branch,
                    git_profile.as_deref(),
                    path,
                    base,
                    chat,
                )
                .await?,
        ),
        WorkspaceMethod::CreateWorkspace {
            path,
            repository,
            name,
            base,
            git_profile,
            chat,
        } => Body::Workspace(
            manager
                .create_workspace_for_chat(
                    repository,
                    name,
                    base,
                    git_profile.as_deref(),
                    path,
                    chat,
                )
                .await?,
        ),
        WorkspaceMethod::ReopenWorkspace { workspace } => {
            Body::OpenedWorkspace(manager.reopen_workspace(&workspace).await?)
        }
        WorkspaceMethod::SplitWorkspace {
            workspace,
            name,
            base,
        } => Body::Workspace(manager.split_workspace(&workspace, name, base).await?),
        WorkspaceMethod::CheckCreation {
            repository,
            branch,
            existing,
            base,
            git_profile,
            path,
        } => {
            manager
                .check_creation(
                    &repository,
                    branch.as_deref(),
                    existing.as_deref(),
                    workspace::NewWorktreeOptions {
                        git_profile: git_profile.as_deref(),
                        path: path.as_deref(),
                        base: base.as_deref(),
                    },
                )
                .await?;
            Body::Ok
        }
        WorkspaceMethod::CheckDependencies { repository, after } => {
            manager.check_dependencies(&repository, after).await?;
            Body::Ok
        }
        WorkspaceMethod::AdoptWorkspace {
            repository,
            path,
            copy,
        } => Body::Workspace(manager.adopt_workspace(&repository, &path, copy).await?),
        WorkspaceMethod::RenameWorkspace { workspace, branch } => Body::Workspace(
            manager
                .rename_workspace(&workspace, &branch, caller.and_then(|c| c.execution_id()))
                .await?,
        ),
        WorkspaceMethod::ListWorkspaces => Body::Workspaces(
            manager
                .visible_workspaces(caller.map(|c| c.workspace_id.as_str()))
                .await?,
        ),
        WorkspaceMethod::UntrackedWorktrees => {
            let listed = manager.untracked_worktrees().await;
            return Ok(Answer::pages(listed, Body::UntrackedWorktrees));
        }
        WorkspaceMethod::SetBaseWorkspace { workspace, base } => {
            Body::Workspace(manager.set_base_workspace(&workspace, base).await?)
        }
        WorkspaceMethod::VerifyWorkspace {
            workspace,
            on_branch,
        } => Body::Workspace(manager.verify_workspace(&workspace, on_branch).await?),
        WorkspaceMethod::FastForwardWorkspace { workspace, branch } => {
            Body::FastForwarded(manager.fast_forward_workspace(&workspace, &branch).await?)
        }
        WorkspaceMethod::InspectWorkspace { workspace } => {
            Body::Inspection(manager.inspect_workspace(&workspace).await?)
        }
        WorkspaceMethod::WorkspaceEnv { workspace } => {
            Body::WorkspaceEnv(manager.workspace_environment(&workspace).await?)
        }
        WorkspaceMethod::RevokeWorkspaceEnv { workspace, token } => {
            manager
                .revoke_workspace_environment(&workspace, token)
                .await?;
            Body::Ok
        }
        WorkspaceMethod::WorkspaceStatus { workspace } => {
            Body::WorkspaceStatus(manager.workspace_status(&workspace).await?)
        }
        WorkspaceMethod::FindWorkspaces { target } => {
            Body::Workspaces(manager.find_visible_workspaces(&target, caller).await?)
        }
        WorkspaceMethod::DiffBase { workspace } => {
            Body::DiffBase(manager.diff_base(&workspace).await?)
        }
        WorkspaceMethod::Conflicts { workspace, target } => {
            Body::Conflicts(manager.check_conflicts(&workspace, target).await?)
        }
        WorkspaceMethod::CloseOwnedPlace {
            workspace,
            host,
            place,
        } => Body::PlaceClose(
            manager
                .close_recorded_place(&workspace, host, &place)
                .await?,
        ),
        WorkspaceMethod::RecordOwnedPlace { workspace, origin } => {
            manager.record_owned_place(&workspace, origin).await?;
            Body::Ok
        }
        WorkspaceMethod::KeepAgentEnvironment {
            workspace,
            environment,
        } => {
            manager
                .keep_agent_environment(&workspace, environment)
                .await?;
            Body::Ok
        }
        WorkspaceMethod::WorkspaceHook { workspace, kind } => {
            Body::Hook(manager.selected_workspace_hook(&workspace, kind).await?)
        }
        WorkspaceMethod::LayeredConfig { target } => {
            Body::LayeredConfig(Box::new(manager.config_layers_for(target).await?))
        }
    }))
}

impl Scoped for WorkspaceMethod {
    async fn scope(&mut self, _: &Manager, _: &Caller) -> Result<Option<&mut String>> {
        Ok(match self {
            WorkspaceMethod::ListWorkspaces | WorkspaceMethod::FindWorkspaces { .. } => None,
            WorkspaceMethod::SetBaseWorkspace { workspace, .. }
            | WorkspaceMethod::RenameWorkspace { workspace, .. }
            | WorkspaceMethod::SplitWorkspace { workspace, .. }
            | WorkspaceMethod::InspectWorkspace { workspace }
            | WorkspaceMethod::VerifyWorkspace { workspace, .. }
            | WorkspaceMethod::FastForwardWorkspace { workspace, .. }
            | WorkspaceMethod::WorkspaceStatus { workspace }
            | WorkspaceMethod::DiffBase { workspace }
            | WorkspaceMethod::Conflicts { workspace, .. }
            | WorkspaceMethod::WorkspaceHook { workspace, .. }
            | WorkspaceMethod::KeepAgentEnvironment { workspace, .. }
            | WorkspaceMethod::RecordOwnedPlace { workspace, .. }
            | WorkspaceMethod::CloseOwnedPlace { workspace, .. }
            | WorkspaceMethod::LayeredConfig {
                target: ConfigTarget::Workspace(workspace),
            } => Some(workspace),
            WorkspaceMethod::OpenBranch { .. }
            | WorkspaceMethod::CreateWorkspace { .. }
            | WorkspaceMethod::ReopenWorkspace { .. }
            | WorkspaceMethod::CheckCreation { .. }
            | WorkspaceMethod::CheckDependencies { .. }
            | WorkspaceMethod::AdoptWorkspace { .. }
            | WorkspaceMethod::WorkspaceEnv { .. }
            | WorkspaceMethod::RevokeWorkspaceEnv { .. }
            | WorkspaceMethod::LayeredConfig {
                target: ConfigTarget::Repository(_),
            }
            | WorkspaceMethod::UntrackedWorktrees
            | WorkspaceMethod::WatchWorkspaceEvents { .. } => return Err(denied()),
        })
    }
}

pub(in crate::daemon) async fn watch_workspace_events(
    mut stream: UnixStream,
    request_id: u64,
    manager: Arc<Manager>,
    since: Option<i64>,
    follow: bool,
    startup: OperationGuard,
) -> Result<()> {
    if since.is_some_and(|id| id < 0) {
        return protocol::write(
            &mut stream,
            &Response::new(
                request_id,
                Body::error(
                    ErrorCode::InvalidRequest,
                    "event cursor must be nonnegative",
                ),
            ),
        )
        .await;
    }
    // Subscribe before the first read so a concurrent commit cannot be missed.
    let mut changed = manager.store.watch_changes();
    let end = manager.latest_workspace_event_id().await?;
    protocol::write(&mut stream, &Response::new(request_id, Body::Ok)).await?;
    let _finite_request = if follow {
        drop(startup);
        None
    } else {
        Some(startup)
    };
    let (mut reader, mut writer) = stream.split();
    let mut cursor = since;
    loop {
        let operation = manager.gates.operation().await;
        let items = manager.workspace_events(cursor, 100).await?;
        let mut delivered = false;
        for item in items {
            if let Some(id) = item.event_id() {
                if !follow && id > end {
                    break;
                }
                delivered = true;
            }
            cursor = Some(item.cursor());
            protocol::write(
                &mut writer,
                &Response::new(request_id, Body::EventItem(item)),
            )
            .await?;
        }
        // Drain the complete backlog before waiting for another commit.
        if delivered && (follow || cursor.is_some_and(|id| id < end)) {
            continue;
        }
        if !follow {
            protocol::write(&mut writer, &Response::new(request_id, Body::Ok)).await?;
            return Ok(());
        }
        drop(operation);
        let mut closed = [0u8; 1];
        tokio::select! {
            result = changed.changed() => result?,
            _ = tokio::io::AsyncReadExt::read(&mut reader, &mut closed) => return Ok(()),
        }
    }
}
