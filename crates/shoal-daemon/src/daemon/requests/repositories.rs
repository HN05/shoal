//! The daemon's answers to [`RepositoryMethod`] requests, and who may send them.
use std::sync::Arc;

use anyhow::Result;

use super::super::{
    answer::Answer,
    permissions::Action,
    scope::{Caller, Scoped},
    workspace::Manager,
};
use crate::client::space_names::SpaceSelector;
use crate::{
    config::permissions::PermissionKey,
    daemon::workspace::spotlight::Layout,
    protocol::{Body, RepositoryMethod},
};

pub(super) async fn handle(
    manager: &Arc<Manager>,
    method: RepositoryMethod,
    caller: Option<&Caller>,
) -> Result<Answer> {
    Ok(Answer::Body(match method {
        RepositoryMethod::AgentEnvironments { repository_id } => {
            let listed = manager.agent_environments(&repository_id).await;
            return Ok(Answer::pages(listed, Body::AgentEnvironments));
        }
        RepositoryMethod::ListRepositories => {
            Body::Repositories(manager.visible_repositories(caller).await?)
        }
        RepositoryMethod::RegisterRepository { source, name, path } => {
            Body::Repository(manager.register_repository(source, name, path).await?)
        }
        RepositoryMethod::RegisterContribution { source, name } => {
            Body::Repository(manager.register_contribution(source, name).await?)
        }
        RepositoryMethod::RenameRepository { repository, name } => {
            Body::Repository(manager.rename_repository(&repository, name).await?)
        }
        RepositoryMethod::LocalConfigPath { repository } => {
            Body::LocalConfigPath(manager.local_config_path(&repository).await?)
        }
        RepositoryMethod::EditLocalConfig {
            repository,
            changes,
        } => Body::LocalConfigEdit(manager.edit_local_config(&repository, changes).await?),
        RepositoryMethod::RemoveRepository { repository } => {
            Body::RepositoryRemoved(manager.remove_repository(&repository).await?)
        }
        RepositoryMethod::PlanRepositoryMoves { root, regular_dir } => Body::RepositoryMoves(
            manager
                .plan_repository_moves(root.as_deref(), Layout::migration(regular_dir))
                .await?,
        ),
        RepositoryMethod::MoveRepositories { regular_dir } => Body::MovedRepositories(
            manager
                .move_repositories(Layout::migration(regular_dir))
                .await?,
        ),
        RepositoryMethod::SyncRepository { repository } => {
            Body::SyncedRepository(manager.sync_repository(&repository).await?)
        }
        RepositoryMethod::FailingSyncs => {
            let failing = manager.failing_syncs().await;
            return Ok(Answer::pages(failing, Body::FailingSyncs));
        }
        RepositoryMethod::ListBranches { repository } => {
            Body::Branches(manager.branches(&repository).await?)
        }
        RepositoryMethod::KeptBranches { repository } => {
            Body::KeptBranches(manager.list_kept_branches(repository.as_deref()).await?)
        }
        RepositoryMethod::DeleteKeptBranches { branches } => {
            Body::KeptBranches(manager.delete_kept_branches(branches).await?)
        }
        RepositoryMethod::KeepRepositoryAgentEnvironment {
            repository,
            environment,
        } => {
            manager
                .keep_repository_agent_environment(&repository, environment)
                .await?;
            Body::Ok
        }
        RepositoryMethod::ForgetAgentEnvironments { repository_id } => {
            manager.forget_agent_environments(&repository_id).await?;
            Body::Ok
        }
    }))
}

/// A scoped caller syncs its own repository; administering repositories is
/// the `admin` permission's, in its own where the request names one.
impl Scoped for RepositoryMethod {
    async fn scope(
        &mut self,
        manager: &Manager,
        caller: &Caller,
    ) -> Result<Option<&mut SpaceSelector>> {
        let repository = match self {
            RepositoryMethod::ListRepositories => return Ok(None),
            // The inbox lists them with the user's notifications.
            RepositoryMethod::FailingSyncs => {
                let action = Action::new(PermissionKey::Notifications, "the failing syncs");
                manager.authorize_action(Some(caller), action).await?;
                return Ok(None);
            }
            RepositoryMethod::SyncRepository { repository } => {
                *repository = manager
                    .own_repository(
                        caller,
                        repository,
                        "workspace processes can only sync their own repository",
                    )
                    .await?;
                return Ok(None);
            }
            RepositoryMethod::RenameRepository { repository, .. }
            | RepositoryMethod::LocalConfigPath { repository }
            | RepositoryMethod::EditLocalConfig { repository, .. }
            | RepositoryMethod::RemoveRepository { repository }
            | RepositoryMethod::ListBranches { repository } => {
                const REFUSAL: &str = "scoped processes administer only their own repository";
                *repository = manager.own_repository(caller, repository, REFUSAL).await?;
                Some(manager.repository(&*repository).await?.id)
            }
            _ => None,
        };
        manager.administer(caller, self, repository).await?;
        Ok(None)
    }
}
