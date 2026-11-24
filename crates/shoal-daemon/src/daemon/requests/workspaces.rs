//! The daemon's answers to [`WorkspaceMethod`] requests, who may send them, and
//! what each requires of the space it acts on.
use shoal_daemon_base::permitted_actions::Action;
use std::sync::Arc;

use anyhow::Result;

use super::super::{
    answer::Answer,
    manager::Manager,
    scope::{Caller, Scoped},
    target_checks::{Checked, TargetCheck},
    workspace,
};
use crate::config::permissions::PermissionKey;
use crate::protocol::{Body, WorkspaceMethod};
use shoal_protocol::{scope_tokens::ScopeToken, space_names::SpaceSelector};

pub(super) async fn handle(
    manager: &Arc<Manager>,
    method: WorkspaceMethod,
    caller: Option<&Caller>,
    scope: Option<&ScopeToken>,
) -> Result<Answer> {
    Ok(Answer::Body(match method {
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
                    .spaces
                    .open_branch_below(
                        &**manager,
                        repository,
                        &branch,
                        base,
                        new,
                        manager.spaces.space_id(parent.as_ref()).await?,
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
                    .spaces
                    .create_workspace_below(
                        &**manager,
                        repository,
                        name,
                        base,
                        new,
                        manager.spaces.space_id(parent.as_ref()).await?,
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
                .spaces
                .create_commit_workspace(
                    &**manager,
                    repository,
                    (manager.spaces.space_id(parent.as_ref()).await?, caller),
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
                .spaces
                .open_pull_head(
                    &**manager,
                    repository,
                    url,
                    head,
                    workspace::NewWorkspace {
                        parent: manager.spaces.space_id(parent.as_ref()).await?,
                        options,
                        ..Default::default()
                    },
                    caller,
                )
                .await?,
        ),
        WorkspaceMethod::ReopenWorkspace { workspace } => Body::OpenedWorkspace(
            manager
                .spaces
                .reopen_workspace(&**manager, &workspace)
                .await?,
        ),
        WorkspaceMethod::CreateChild {
            parent,
            name,
            base,
            options,
        } => Body::Workspace(
            manager
                .spaces
                .create_child(&**manager, &parent, name, base, options)
                .await?,
        ),
        WorkspaceMethod::AdoptWorkspace {
            repository,
            path,
            copy,
        } => Body::Workspace(
            manager
                .spaces
                .adopt_workspace(&**manager, &repository, &path, copy)
                .await?,
        ),
        WorkspaceMethod::RenameBranch { workspace, branch } => Body::Workspace(
            manager
                .spaces
                .rename_branch(
                    &**manager,
                    &workspace,
                    &branch,
                    caller.and_then(|c| c.execution_id()),
                )
                .await?,
        ),
        WorkspaceMethod::UntrackedWorktrees => {
            let listed = manager.spaces.untracked_worktrees(&**manager).await;
            return Ok(Answer::pages(listed, Body::UntrackedWorktrees));
        }
        WorkspaceMethod::SetBaseWorkspace { workspace, base } => {
            Body::Workspace(manager.spaces.set_base_workspace(&workspace, base).await?)
        }
        WorkspaceMethod::VerifyWorkspace {
            workspace,
            on_branch,
        } => Body::Workspace(
            manager
                .spaces
                .verify_workspace(&**manager, &workspace, on_branch)
                .await?,
        ),
        WorkspaceMethod::FastForwardWorkspace { workspace, branch } => Body::FastForwarded(
            manager
                .spaces
                .fast_forward_workspace(&**manager, &workspace, &branch)
                .await?,
        ),
        WorkspaceMethod::RefreshWorkspace { workspace } => Body::Refreshed(
            manager
                .spaces
                .refresh_workspace(&**manager, &workspace, caller)
                .await?,
        ),
        WorkspaceMethod::InspectWorkspace { workspace } => {
            let mut inspection = manager
                .spaces
                .inspect_workspace(&**manager, &workspace)
                .await?;
            inspection.machine = super::forwarded::answering_machine(manager, scope).await?;
            Body::Inspection(inspection)
        }
        WorkspaceMethod::CopyOrigin { source } => {
            Body::CopyOrigin(manager.spaces.copy_origin(&source).await?)
        }
        WorkspaceMethod::KeepAgentEnvironment {
            workspace,
            environment,
        } => {
            manager
                .agents
                .keep_agent_environment(&**manager, &workspace, environment)
                .await?;
            Body::Ok
        }
        WorkspaceMethod::CreatePendingWorktree { workspace } => Body::Workspace(
            manager
                .spaces
                .create_pending_worktree(&**manager, &workspace)
                .await?,
        ),
        WorkspaceMethod::WorkspaceHook { workspace, kind } => Body::Hook(
            manager
                .spaces
                .selected_workspace_hook(&**manager, &workspace, kind)
                .await?,
        ),
    }))
}

impl Checked for WorkspaceMethod {
    fn target_check(&self) -> Option<TargetCheck> {
        match self {
            Self::SetBaseWorkspace { base: None, .. } => Some(TargetCheck::StackedWorkspace),
            Self::SetBaseWorkspace { base: Some(_), .. } => Some(TargetCheck::Workspace),
            Self::ReopenWorkspace { .. }
            | Self::FastForwardWorkspace { .. }
            | Self::RefreshWorkspace { .. } => Some(TargetCheck::ReadyWorkspace),
            Self::RenameBranch { .. } => Some(TargetCheck::SettledWorkspace),
            // Only a ready workspace's current files are copied.
            Self::CopyOrigin { source } => Some(match source.checkpoint {
                None => TargetCheck::ReadyWorkspace,
                Some(_) => TargetCheck::Workspace,
            }),
            Self::InspectWorkspace { .. }
            | Self::VerifyWorkspace { .. }
            | Self::KeepAgentEnvironment { .. }
            | Self::CreatePendingWorktree { .. }
            | Self::WorkspaceHook { .. } => Some(TargetCheck::Workspace),
            Self::OpenBranch { .. }
            | Self::CreateWorkspace { .. }
            | Self::CreateChild { .. }
            | Self::CreateCommitWorkspace { .. }
            | Self::OpenPullHead { .. }
            | Self::AdoptWorkspace { .. }
            | Self::UntrackedWorktrees => None,
        }
    }
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
            manager
                .coordination
                .copy_within(manager, caller, source)
                .await?;
        }
        Ok(match self {
            // A copy's source is confined as a copy that starts a space is,
            // which tells a scoped caller what to do about a removed one.
            WorkspaceMethod::CopyOrigin { source } => {
                manager
                    .coordination
                    .copy_within(manager, caller, source)
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
            WorkspaceMethod::SetBaseWorkspace { workspace, .. }
            | WorkspaceMethod::RenameBranch { workspace, .. }
            | WorkspaceMethod::CreateChild {
                parent: workspace, ..
            }
            | WorkspaceMethod::CreateCommitWorkspace {
                parent: Some(workspace),
                ..
            }
            | WorkspaceMethod::InspectWorkspace { workspace }
            | WorkspaceMethod::VerifyWorkspace { workspace, .. }
            | WorkspaceMethod::FastForwardWorkspace { workspace, .. }
            | WorkspaceMethod::RefreshWorkspace { workspace }
            | WorkspaceMethod::WorkspaceHook { workspace, .. }
            | WorkspaceMethod::CreatePendingWorktree { workspace }
            | WorkspaceMethod::KeepAgentEnvironment { workspace, .. } => Some(workspace),
            // The inbox lists them with the user's notifications.
            WorkspaceMethod::UntrackedWorktrees => {
                let action = Action::new(PermissionKey::Notifications, "the untracked worktrees");
                manager
                    .coordination
                    .authorize_action(manager, Some(caller), action)
                    .await?;
                None
            }
            WorkspaceMethod::ReopenWorkspace { .. } | WorkspaceMethod::AdoptWorkspace { .. } => {
                administer(manager, caller, self).await?
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
            *repository = manager
                .coordination
                .own_repository(manager, caller, repository, REFUSAL)
                .await?;
            Some(manager.repositories.repository(&*repository).await?.id)
        }
        _ => None,
    };
    manager
        .coordination
        .ensure_selected_reached(manager, caller, administered(method))
        .await?;
    let repository = match (administered(method), repository) {
        (Some(space), None) => {
            manager
                .coordination
                .space_repository(manager, space)
                .await?
        }
        (_, repository) => repository,
    };
    manager
        .coordination
        .administer(manager, caller, &*method, repository)
        .await?;
    Ok(administered(method))
}

/// The space `method`, which the `admin` permission governs, acts on.
fn administered(method: &mut WorkspaceMethod) -> Option<&mut SpaceSelector> {
    match method {
        WorkspaceMethod::ReopenWorkspace { workspace } => Some(workspace),
        _ => None,
    }
}
