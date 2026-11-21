//! The daemon's answers to [`WorkspaceMethod`] requests, and who may send them.
use shoal_daemon_base::permitted_actions::Action;
use std::sync::Arc;

use anyhow::{Context as _, Result, ensure};
use tokio::net::UnixStream;

use super::super::{
    answer::{Answer, OperationGuard},
    manager::Manager,
    scope::{Caller, Scoped, denied},
    workspace,
};
use crate::config::permissions::PermissionKey;
use crate::protocol::{
    self, Body, ConfigTarget, ErrorCode, RepositoryLayers, Response, WorkspaceMethod,
};
use shoal_protocol::{events::EventCursor, scope_tokens::ScopeToken, space_names::SpaceSelector};

pub(super) async fn handle(
    manager: &Arc<Manager>,
    method: WorkspaceMethod,
    caller: Option<&Caller>,
    scope: Option<&ScopeToken>,
) -> Result<Answer> {
    Ok(Answer::Body(match method {
        WorkspaceMethod::WatchSpaceEvents { after, follow } => {
            let manager = manager.clone();
            return Ok(Answer::connection(move |stream, request_id, guard| {
                watch_space_events(stream, request_id, manager, after, follow, guard)
            }));
        }
        WorkspaceMethod::SpaceLog {
            space,
            filter,
            limit,
        } => {
            let log = manager.log(space.as_ref(), filter, limit).await;
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
                        caller,
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
                    (manager.space_id(parent.as_ref()).await?, caller),
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
                    workspace::NewWorkspace {
                        parent: manager.space_id(parent.as_ref()).await?,
                        options,
                        ..Default::default()
                    },
                    caller,
                )
                .await?,
        ),
        WorkspaceMethod::ReopenWorkspace { workspace } => {
            Body::OpenedWorkspace(manager.reopen_workspace(&workspace).await?)
        }
        WorkspaceMethod::CreateChild {
            parent,
            name,
            base,
            options,
        } => Body::Workspace(manager.create_child(&parent, name, base, options).await?),
        WorkspaceMethod::AdoptWorkspace {
            repository,
            path,
            copy,
        } => Body::Workspace(manager.adopt_workspace(&repository, &path, copy).await?),
        WorkspaceMethod::RenameBranch { workspace, branch } => Body::Workspace(
            manager
                .rename_branch(&workspace, &branch, caller.and_then(|c| c.execution_id()))
                .await?,
        ),
        WorkspaceMethod::ConvertToChat { workspace, mode } => Body::Workspace(
            manager
                .convert_to_chat(&workspace, mode.unwrap_or_else(crate::model::chat_mode))
                .await?,
        ),
        WorkspaceMethod::ListSpaces => Body::Spaces(manager.listed_spaces(caller).await?),
        WorkspaceMethod::UntrackedWorktrees => {
            let listed = manager.untracked_worktrees().await;
            return Ok(Answer::pages(listed, Body::UntrackedWorktrees));
        }
        WorkspaceMethod::SetBaseWorkspace { workspace, base } => {
            Body::Workspace(manager.set_base_workspace(&workspace, base).await?)
        }
        WorkspaceMethod::RenameSpace { space, name } => {
            Body::RenamedSpace(manager.rename_space(&space, &name).await?)
        }
        WorkspaceMethod::VerifyWorkspace {
            workspace,
            on_branch,
        } => Body::Workspace(manager.verify_workspace(&workspace, on_branch).await?),
        WorkspaceMethod::FastForwardWorkspace { workspace, branch } => {
            Body::FastForwarded(manager.fast_forward_workspace(&workspace, &branch).await?)
        }
        WorkspaceMethod::RefreshWorkspace { workspace } => {
            Body::Refreshed(manager.refresh_workspace(&workspace, caller).await?)
        }
        WorkspaceMethod::InspectWorkspace { workspace } => {
            Body::Inspection(manager.inspect_workspace(&workspace).await?)
        }
        WorkspaceMethod::LaunchPermissions {
            workspace,
            mode,
            narrowing,
        } => {
            let workspace = manager.workspace(&workspace).await?;
            Body::Permissions(
                manager
                    .launch_permissions(workspace, &mode, narrowing)
                    .await?,
            )
        }
        WorkspaceMethod::HeldPermissions { repository } => {
            let caller = caller.context("the user holds every permission")?;
            let repository = match repository {
                Some(repository) => Some(manager.repository(repository).await?.id),
                None => None,
            };
            let held = manager.caller_permissions(caller, repository.as_ref());
            Body::Permissions(held.await?)
        }
        WorkspaceMethod::IssueScope { space } => {
            Body::ScopeToken(manager.issue_scope(&space).await?)
        }
        WorkspaceMethod::RevokeScope { space, token } => {
            manager.revoke_scope(&space, token).await?;
            Body::Ok
        }
        WorkspaceMethod::SpaceEnvironment { space } => {
            Body::SpaceEnvironment(manager.space_environment(&space).await?)
        }
        WorkspaceMethod::SpaceStatus { space } => {
            let mut status = manager.space_status(&space).await?;
            // Another machine asked for it, whose caller learns where it runs.
            if scope.is_some_and(|token| manager.hub.vouched_caller(token).is_some()) {
                status.machine = manager.hub.this_machine(&**manager).await?;
            }
            Body::SpaceStatus(Box::new(status))
        }
        WorkspaceMethod::TrackedConfig { space } => {
            Body::TrackedConfig(manager.space_tracked_config(&space).await?)
        }
        WorkspaceMethod::SpaceSessions { space } => {
            Body::SpaceSessions(manager.space_sessions(&space).await?)
        }
        WorkspaceMethod::FollowSession {
            space,
            session,
            from,
        } => {
            // Chosen first, so a space another machine has is followed there.
            // A client follows again, from where it read, on its own.
            let resumed = !from.is_empty();
            let following = manager.session_to_follow(&space, session, from).await?;
            // Another machine's follow is checked there as it goes, its
            // token here standing for its caller only while it starts.
            let scope = scope.filter(|token| manager.hub.vouched_caller(token).is_none());
            let scope = scope.cloned();
            // Following an agent is the user's interaction with it, as
            // attaching is.
            if !resumed {
                manager.record_user_interaction(caller, &space).await;
            }
            let manager = manager.clone();
            return Ok(Answer::connection(move |stream, request_id, guard| {
                manager.follow_session(stream, request_id, (following, scope), guard)
            }));
        }
        WorkspaceMethod::AttachSession { space, viewer } => {
            let terminal = manager.open_viewer_terminal(&space, &viewer).await?;
            let manager = manager.clone();
            return Ok(Answer::connection(move |stream, request_id, guard| {
                let answer = (request_id, Body::Ok);
                let until = std::future::pending();
                manager.relay_viewer_terminal(stream, answer, terminal, until, guard)
            }));
        }
        WorkspaceMethod::CopySpace {
            space,
            checkpoint,
            file,
            carrying,
        } => Body::ExportedSpace(
            manager
                .copy_space(&space, checkpoint, file, &carrying)
                .await?,
        ),
        WorkspaceMethod::FindSpaces { target } => {
            Body::Spaces(manager.find_visible_spaces(&target, caller).await?)
        }
        WorkspaceMethod::DiffBase { workspace } => {
            Body::DiffBase(manager.diff_base(&workspace).await?)
        }
        WorkspaceMethod::Conflicts { workspace, target } => {
            Body::Conflicts(manager.check_conflicts(&workspace, target).await?)
        }
        WorkspaceMethod::Diff { workspace, display } => {
            let diff = manager.start_diff(&workspace, display).await?;
            let manager = manager.clone();
            return Ok(Answer::connection(move |stream, request_id, guard| {
                manager.stream_diff(stream, request_id, diff, guard)
            }));
        }
        WorkspaceMethod::PreviewMerge {
            workspace,
            push,
            method,
            message,
        } => {
            let options = crate::model::MergeOptions {
                push,
                method,
                message,
                confirmed: None,
            };
            Body::MergePreview(manager.preview_merge(&workspace, &options, caller).await?)
        }
        WorkspaceMethod::CloseOwnedPlace { workspace, place } => {
            Body::PlaceClose(manager.close_recorded_place(&workspace, &place).await?)
        }
        WorkspaceMethod::PlaceOwnership { space, place } => {
            Body::PlaceOwnership(manager.place_ownership(&space, &place).await?)
        }
        WorkspaceMethod::OwnedPlaces => owned_places(manager).await?,
        WorkspaceMethod::SpacePlaces { space } => {
            Body::SpacePlaces(manager.space_places(&space).await?)
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
        WorkspaceMethod::CreatePendingWorktree { workspace } => {
            Body::Workspace(manager.create_pending_worktree(&workspace).await?)
        }
        WorkspaceMethod::WorkspaceHook { workspace, kind } => {
            Body::Hook(manager.selected_workspace_hook(&workspace, kind).await?)
        }
        WorkspaceMethod::LayeredConfig { target } => Body::LayeredConfig(RepositoryLayers::of(
            &manager.config_layers_for(target).await?,
        )?),
    }))
}

/// The places Shoal created for every space.
async fn owned_places(manager: &Manager) -> Result<Body> {
    Ok(Body::OwnedPlaces(
        manager
            .owned_places(None)
            .await?
            .into_iter()
            .map(|(space_id, origin)| crate::model::OwnedPlace { space_id, origin })
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
        // A copy for a joined machine, which goes among the packages
        // delivered from here, as a move there, needs no permission key.
        if let WorkspaceMethod::CopySpace { space, file, .. } = &*self
            && manager
                .copies_for_joined_machine(caller, space, file)
                .await?
        {
            return Ok(administered(self));
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
            // The workspace may be removed already, so its ID is checked as
            // given instead of resolved.
            WorkspaceMethod::CloseOwnedPlace { workspace, .. } => {
                ensure!(
                    manager.reaches(caller, workspace).await?,
                    "workspace processes close only the places of their own space and those below it"
                );
                None
            }
            // A coordinator follows the spaces below it, removed ones too.
            WorkspaceMethod::SpaceLog { space, .. } => {
                manager.narrow_to_log(caller, space).await?;
                None
            }
            WorkspaceMethod::LayeredConfig {
                target: ConfigTarget::Global,
            } => None,
            WorkspaceMethod::ListSpaces | WorkspaceMethod::FindSpaces { .. } => None,
            // The handler answers with the caller's own space's.
            WorkspaceMethod::HeldPermissions { .. } => None,
            WorkspaceMethod::SetBaseWorkspace { workspace, .. }
            | WorkspaceMethod::SpacePlaces { space: workspace }
            | WorkspaceMethod::RenameBranch { workspace, .. }
            | WorkspaceMethod::ConvertToChat { workspace, .. }
            | WorkspaceMethod::CreateChild {
                parent: workspace, ..
            }
            | WorkspaceMethod::CreateCommitWorkspace {
                parent: Some(workspace),
                ..
            }
            | WorkspaceMethod::InspectWorkspace { workspace }
            | WorkspaceMethod::VerifyWorkspace { workspace, .. }
            | WorkspaceMethod::LaunchPermissions { workspace, .. }
            | WorkspaceMethod::FastForwardWorkspace { workspace, .. }
            | WorkspaceMethod::RefreshWorkspace { workspace }
            | WorkspaceMethod::DiffBase { workspace }
            | WorkspaceMethod::Conflicts { workspace, .. }
            | WorkspaceMethod::Diff { workspace, .. }
            | WorkspaceMethod::PreviewMerge { workspace, .. }
            | WorkspaceMethod::WorkspaceHook { workspace, .. }
            | WorkspaceMethod::CreatePendingWorktree { workspace }
            | WorkspaceMethod::KeepAgentEnvironment { workspace, .. }
            | WorkspaceMethod::RecordOwnedPlace { workspace, .. }
            | WorkspaceMethod::LayeredConfig {
                target: ConfigTarget::Space(workspace),
            } => Some(workspace),
            WorkspaceMethod::RenameSpace { space, .. }
            | WorkspaceMethod::SpaceStatus { space }
            | WorkspaceMethod::TrackedConfig { space }
            | WorkspaceMethod::SpaceSessions { space }
            | WorkspaceMethod::FollowSession { space, .. }
            | WorkspaceMethod::PlaceOwnership { space, .. } => Some(space),
            // A token for a space its scope reaches acts for no more than
            // the caller's own.
            WorkspaceMethod::IssueScope { space }
            | WorkspaceMethod::RevokeScope { space, .. }
            | WorkspaceMethod::SpaceEnvironment { space } => Some(space),
            // The inbox lists them with the user's notifications.
            WorkspaceMethod::UntrackedWorktrees => {
                let action = Action::new(PermissionKey::Notifications, "the untracked worktrees");
                manager.authorize_action(Some(caller), action).await?;
                None
            }
            WorkspaceMethod::ReopenWorkspace { .. }
            | WorkspaceMethod::AdoptWorkspace { .. }
            | WorkspaceMethod::CopySpace { .. } => administer(manager, caller, self).await?,
            WorkspaceMethod::OwnedPlaces | WorkspaceMethod::WatchSpaceEvents { .. } => {
                return Err(denied());
            }
            // As `shoal attach` is in a scoped execution.
            WorkspaceMethod::AttachSession { .. } => {
                anyhow::bail!("attach to sessions outside scoped executions")
            }
        })
    }
}

/// Let `caller` send `method`, which the `admin` permission governs, in its
/// own repository where it adopts a worktree there, returning the space it
/// acts on, which its scope must reach before it asks approval.
async fn administer<'a>(
    manager: &Manager,
    caller: &Caller,
    method: &'a mut WorkspaceMethod,
) -> Result<Option<&'a mut SpaceSelector>> {
    let repository = match method {
        WorkspaceMethod::AdoptWorkspace { repository, .. } => {
            const REFUSAL: &str = "scoped processes adopt worktrees only in their own repository";
            *repository = manager.own_repository(caller, repository, REFUSAL).await?;
            Some(manager.repository(&*repository).await?.id)
        }
        _ => None,
    };
    manager
        .ensure_selected_reached(caller, administered(method))
        .await?;
    let repository = match (administered(method), repository) {
        (Some(space), None) => manager.space_repository(space).await?,
        (_, repository) => repository,
    };
    manager.administer(caller, &*method, repository).await?;
    Ok(administered(method))
}

/// The space `method`, which the `admin` permission governs, acts on.
fn administered(method: &mut WorkspaceMethod) -> Option<&mut SpaceSelector> {
    match method {
        WorkspaceMethod::ReopenWorkspace { workspace } => Some(workspace),
        WorkspaceMethod::CopySpace { space, .. } => Some(space),
        _ => None,
    }
}

pub(in crate::daemon) async fn watch_space_events(
    mut stream: UnixStream,
    request_id: u64,
    manager: Arc<Manager>,
    after: Option<EventCursor>,
    follow: bool,
    startup: OperationGuard,
) -> Result<()> {
    if after.is_some_and(|cursor| cursor.id() < 0) {
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
    let end = crate::daemon::events::latest_id(&manager.store).await?;
    protocol::write(&mut stream, &Response::new(request_id, Body::Ok)).await?;
    let _finite_request = if follow {
        drop(startup);
        None
    } else {
        Some(startup)
    };
    let (mut reader, mut writer) = stream.split();
    let mut cursor = after;
    let mut pruned = None;
    loop {
        let operation = manager.gates.operation().await;
        let (items, now_pruned) =
            (crate::daemon::events::page(&manager.store, cursor, pruned, 100)).await?;
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
