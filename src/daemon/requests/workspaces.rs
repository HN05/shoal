//! The daemon's answers to [`WorkspaceMethod`] requests, and who may send them.
use std::sync::Arc;

use anyhow::Result;
use tokio::net::UnixStream;

use super::super::{
    answer::{Answer, OperationGuard},
    scope::{Caller, Scoped, denied},
    workspace::{self, Manager},
};
use crate::client::space_names::SpaceSelector;
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
        WorkspaceMethod::SpaceLog {
            space,
            filter,
            limit,
        } => {
            let log = manager.space_log(&space, filter, limit).await;
            return Ok(Answer::pages(log, Body::LogEntries));
        }
        WorkspaceMethod::OpenBranch {
            path,
            repository,
            branch,
            git_profile,
            base,
            parent,
            options,
        } => {
            let new = workspace::NewWorkspace {
                git_profile: git_profile.as_deref(),
                path,
                options,
                ..Default::default()
            };
            Body::OpenedWorkspace(
                manager
                    .open_branch_below(
                        repository,
                        &branch,
                        base,
                        new,
                        manager.space_id(parent.as_ref()).await?,
                        caller.is_some(),
                    )
                    .await?,
            )
        }
        WorkspaceMethod::CreateWorkspace {
            path,
            repository,
            name,
            base,
            git_profile,
            parent,
            options,
        } => {
            let new = workspace::NewWorkspace {
                git_profile: git_profile.as_deref(),
                path,
                options,
                ..Default::default()
            };
            Body::Workspace(
                manager
                    .create_workspace_below(
                        repository,
                        name,
                        base,
                        new,
                        manager.space_id(parent.as_ref()).await?,
                    )
                    .await?,
            )
        }
        WorkspaceMethod::CreateCommitWorkspace {
            repository,
            parent,
            start,
            options,
        } => Body::Workspace(
            manager
                .create_commit_workspace(
                    repository,
                    manager.space_id(parent.as_ref()).await?,
                    start,
                    options,
                )
                .await?,
        ),
        WorkspaceMethod::OpenPullHead {
            repository,
            url,
            head,
            parent,
            options,
        } => Body::OpenedWorkspace(
            manager
                .open_pull_head(
                    repository,
                    url,
                    head,
                    manager.space_id(parent.as_ref()).await?,
                    options,
                    caller.is_some(),
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
            options,
        } => Body::Workspace(
            manager
                .split_workspace(&workspace, name, base, options)
                .await?,
        ),
        WorkspaceMethod::CheckCreation {
            repository,
            branch,
            existing,
            base,
            git_profile,
            path,
            options,
            parent,
        } => {
            let parent = manager.space_id(parent.as_ref()).await?;
            (manager.ensure_parent_repository(&repository, parent.as_deref())).await?;
            manager
                .check_creation(
                    &repository,
                    branch.as_deref(),
                    existing.as_deref(),
                    workspace::NewWorktreeOptions {
                        git_profile: git_profile.as_deref(),
                        path: path.as_deref(),
                        base: base.as_deref(),
                        disposable: options.disposable,
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
        WorkspaceMethod::ListWorkspaces => {
            let root = manager.reach_root(caller).await?;
            Body::Workspaces(manager.visible_workspaces(root.as_deref()).await?)
        }
        WorkspaceMethod::UntrackedWorktrees => {
            let listed = manager.untracked_worktrees().await;
            return Ok(Answer::pages(listed, Body::UntrackedWorktrees));
        }
        WorkspaceMethod::SetBaseWorkspace { workspace, base } => {
            Body::Workspace(manager.set_base_workspace(&workspace, base).await?)
        }
        WorkspaceMethod::SetWorkspaceTitle { workspace, title } => {
            Body::RetitledWorkspace(manager.set_workspace_title(&workspace, title).await?)
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
        WorkspaceMethod::LaunchPermissions { workspace, mode } => {
            let workspace = manager.workspace(&workspace).await?;
            Body::Permissions(manager.launch_permissions(workspace, &mode).await?)
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
        WorkspaceMethod::SpaceStatus { space } => {
            Body::SpaceStatus(Box::new(manager.space_status(&space).await?))
        }
        WorkspaceMethod::SpaceSessions { space } => {
            Body::SpaceSessions(manager.space_sessions(&space).await?)
        }
        WorkspaceMethod::CopySpace {
            space,
            checkpoint,
            file,
        } => Body::ExportedSpace(manager.copy_space(&space, checkpoint, file).await?),
        WorkspaceMethod::FindSpaces { target } => {
            Body::Spaces(manager.find_visible_spaces(&target, caller).await?)
        }
        WorkspaceMethod::DiffBase { workspace } => {
            Body::DiffBase(manager.diff_base(&workspace).await?)
        }
        WorkspaceMethod::Conflicts { workspace, target } => {
            Body::Conflicts(manager.check_conflicts(&workspace, target).await?)
        }
        WorkspaceMethod::PreviewMerge { workspace, push } => {
            Body::MergePreview(manager.preview_merge(&workspace, push).await?)
        }
        WorkspaceMethod::CloseOwnedPlace { workspace, place } => {
            // The workspace may be removed already, so its ID stays as given.
            let workspace = match workspace {
                SpaceSelector::Number(_) => manager.workspace(&workspace).await?.id,
                SpaceSelector::IdOrName(id) | SpaceSelector::Id(id) => id,
            };
            Body::PlaceClose(manager.close_recorded_place(&workspace, &place).await?)
        }
        WorkspaceMethod::PlaceOwnership { workspace, place } => {
            Body::PlaceOwnership(manager.place_ownership(&workspace, &place).await?)
        }
        WorkspaceMethod::OwnedPlaces => owned_places(manager, None).await?,
        WorkspaceMethod::WorkspacePlaces { workspace } => {
            let workspace = manager.workspace(&workspace).await?.id;
            owned_places(manager, Some(workspace)).await?
        }
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

/// The places Shoal created for `workspace`, or for every workspace.
async fn owned_places(manager: &Manager, workspace: Option<String>) -> Result<Body> {
    Ok(Body::OwnedPlaces(
        manager
            .owned_places(workspace)
            .await?
            .into_iter()
            .map(|(workspace_id, origin)| crate::model::OwnedPlace {
                workspace_id,
                origin,
            })
            .collect(),
    ))
}

impl Scoped for WorkspaceMethod {
    async fn scope(
        &mut self,
        manager: &Manager,
        caller: &Caller,
    ) -> Result<Option<&mut SpaceSelector>> {
        if let WorkspaceMethod::CreateCommitWorkspace {
            start: crate::protocol::CommitStart::Copy(source),
            ..
        } = self
        {
            manager.copy_within(caller, source).await?;
        }
        Ok(match self {
            // The settings of its own repository choose what it opens there.
            WorkspaceMethod::LayeredConfig {
                target: ConfigTarget::Repository(repository),
            } => {
                *repository = manager
                    .own_repository(
                        caller,
                        repository,
                        "scoped processes read only their own repository's config",
                    )
                    .await?;
                None
            }
            // A scoped caller opens workspaces only below its own space,
            // which must be in their repository.
            WorkspaceMethod::OpenBranch { parent, .. }
            | WorkspaceMethod::CreateWorkspace { parent, .. }
            | WorkspaceMethod::CreateCommitWorkspace {
                parent: parent @ None,
                ..
            }
            | WorkspaceMethod::OpenPullHead { parent, .. } => {
                *parent = Some(SpaceSelector::id(&caller.space_id));
                None
            }
            // A start it checks goes where the start would: below its own
            // space unless it names one below it.
            WorkspaceMethod::CheckCreation { parent, .. } => {
                manager.parent_below(caller, parent).await?;
                None
            }
            // A coordinator follows the spaces below it, removed ones too.
            WorkspaceMethod::SpaceLog { space, .. } => {
                manager.narrow_to_log(caller, space).await?;
                None
            }
            WorkspaceMethod::CheckDependencies { .. }
            | WorkspaceMethod::LayeredConfig {
                target: ConfigTarget::Global,
            } => None,
            WorkspaceMethod::ListWorkspaces | WorkspaceMethod::FindSpaces { .. } => None,
            WorkspaceMethod::SetBaseWorkspace { workspace, .. }
            | WorkspaceMethod::SetWorkspaceTitle { workspace, .. }
            | WorkspaceMethod::WorkspacePlaces { workspace }
            | WorkspaceMethod::RenameWorkspace { workspace, .. }
            | WorkspaceMethod::SplitWorkspace { workspace, .. }
            | WorkspaceMethod::CreateCommitWorkspace {
                parent: Some(workspace),
                ..
            }
            | WorkspaceMethod::InspectWorkspace { workspace }
            | WorkspaceMethod::VerifyWorkspace { workspace, .. }
            | WorkspaceMethod::LaunchPermissions { workspace, .. }
            | WorkspaceMethod::FastForwardWorkspace { workspace, .. }
            | WorkspaceMethod::DiffBase { workspace }
            | WorkspaceMethod::Conflicts { workspace, .. }
            | WorkspaceMethod::PreviewMerge { workspace, .. }
            | WorkspaceMethod::WorkspaceHook { workspace, .. }
            | WorkspaceMethod::KeepAgentEnvironment { workspace, .. }
            | WorkspaceMethod::RecordOwnedPlace { workspace, .. }
            | WorkspaceMethod::CloseOwnedPlace { workspace, .. }
            | WorkspaceMethod::PlaceOwnership { workspace, .. }
            | WorkspaceMethod::LayeredConfig {
                target: ConfigTarget::Workspace(workspace),
            } => Some(workspace),
            WorkspaceMethod::SpaceStatus { space } | WorkspaceMethod::SpaceSessions { space } => {
                Some(space)
            }
            WorkspaceMethod::ReopenWorkspace { .. }
            | WorkspaceMethod::AdoptWorkspace { .. }
            | WorkspaceMethod::WorkspaceEnv { .. }
            | WorkspaceMethod::RevokeWorkspaceEnv { .. }
            | WorkspaceMethod::UntrackedWorktrees
            | WorkspaceMethod::OwnedPlaces
            | WorkspaceMethod::CopySpace { .. }
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
    let mut pruned = None;
    loop {
        let operation = manager.gates.operation().await;
        let (items, now_pruned) = (manager.workspace_events_page(cursor, pruned, 100)).await?;
        pruned = Some(now_pruned);
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
