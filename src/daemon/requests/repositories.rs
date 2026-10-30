//! The daemon's answers to [`RepositoryMethod`] requests, and who may send them.
use std::sync::Arc;

use anyhow::Result;

use super::super::{
    answer::Answer,
    scope::{Caller, Scoped, denied},
    workspace::Manager,
};
use crate::{
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

impl Scoped for RepositoryMethod {
    async fn scope(&mut self, manager: &Manager, caller: &Caller) -> Result<Option<&mut String>> {
        Ok(match self {
            RepositoryMethod::ListRepositories => None,
            RepositoryMethod::SyncRepository { repository } => {
                *repository = manager.own_repository(caller, repository).await?;
                None
            }
            RepositoryMethod::RegisterRepository { .. }
            | RepositoryMethod::RegisterContribution { .. }
            | RepositoryMethod::RenameRepository { .. }
            | RepositoryMethod::LocalConfigPath { .. }
            | RepositoryMethod::EditLocalConfig { .. }
            | RepositoryMethod::RemoveRepository { .. }
            | RepositoryMethod::PlanRepositoryMoves { .. }
            | RepositoryMethod::MoveRepositories { .. }
            | RepositoryMethod::ListBranches { .. }
            | RepositoryMethod::KeptBranches { .. }
            | RepositoryMethod::DeleteKeptBranches { .. }
            | RepositoryMethod::AgentEnvironments { .. }
            | RepositoryMethod::ForgetAgentEnvironments { .. }
            | RepositoryMethod::KeepRepositoryAgentEnvironment { .. } => return Err(denied()),
        })
    }
}
