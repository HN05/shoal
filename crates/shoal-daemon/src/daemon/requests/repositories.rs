//! The daemon's answers to [`RepositoryMethod`] requests, who may send them,
//! and what each requires of the space it acts on.
use shoal_daemon_base::permitted_actions::Action;
use std::sync::Arc;

use anyhow::Result;

use super::super::{
    answer::Answer,
    contribution_holds,
    manager::Manager,
    scope::{Caller, Scoped},
    target_checks::{Checked, TargetCheck},
};
use crate::{
    config::permissions::PermissionKey,
    protocol::{Body, RepositoryMethod},
};
use shoal_ids::repository_ids::RepositoryId;
use shoal_protocol::space_names::SpaceSelector;
use shoal_repositories::spotlight::Layout;

pub(super) async fn handle(
    manager: &Arc<Manager>,
    method: RepositoryMethod,
    caller: Option<&Caller>,
) -> Result<Answer> {
    Ok(Answer::Body(match method {
        RepositoryMethod::ListRepositories => {
            Body::Repositories(manager.visible_repositories(caller).await?)
        }
        RepositoryMethod::RepositoriesElsewhere => {
            Body::RepositoriesElsewhere(manager.visible_repositories_elsewhere(caller).await?)
        }
        RepositoryMethod::RegisterRepository {
            source,
            name,
            path,
            shared,
        } => {
            if shared {
                manager.repositories.ensure_shareable(&source).await?;
            }
            let repository = manager.repositories.register(source, name, path).await?;
            Body::Repository(match shared {
                true => manager.share_repository(&repository.id, true).await?,
                false => repository,
            })
        }
        RepositoryMethod::RegisterContribution { source, name } => {
            let manager = manager.clone();
            return Ok(Answer::connection(move |stream, request_id, guard| {
                contribution_holds::register(stream, request_id, manager, (source, name), guard)
            }));
        }
        RepositoryMethod::ReceiveCommits {
            origin,
            commit,
            branch,
        } => {
            let source = origin.clone().into();
            let repository = (manager.repositories.register(source, None, None)).await?;
            let manager = manager.clone();
            let carried = (repository, origin, commit, branch);
            return Ok(Answer::connection(move |stream, request_id, guard| {
                manager.receive_commits(stream, request_id, carried, guard)
            }));
        }
        RepositoryMethod::HoldContribution { repository } => {
            let manager = manager.clone();
            return Ok(Answer::connection(move |stream, request_id, guard| {
                contribution_holds::hold(stream, request_id, manager, repository, guard)
            }));
        }
        RepositoryMethod::RenameRepository { repository, name } => {
            Body::Repository(manager.repositories.rename(&repository, name).await?)
        }
        RepositoryMethod::ShareRepository { repository, shared } => {
            Body::Repository(manager.share_repository(&repository, shared).await?)
        }
        RepositoryMethod::LocalConfigPath { repository } => {
            Body::LocalConfigPath(manager.repositories.local_config_path(&repository).await?)
        }
        RepositoryMethod::EditLocalConfig {
            repository,
            changes,
        } => Body::LocalConfigEdit(
            manager
                .repositories
                .edit_local_config(&**manager, &repository, changes)
                .await?,
        ),
        RepositoryMethod::RemoveRepository {
            repository,
            machine,
        } => Body::RepositoryRemoved(
            manager
                .remove_repository(repository, machine.as_deref(), caller)
                .await?,
        ),
        RepositoryMethod::PreviewRepositoryRemoval {
            repository,
            machine,
        } => Body::RepositoryRemovalPreview(
            manager
                .preview_repository_removal(repository, machine.as_deref(), caller)
                .await?,
        ),
        RepositoryMethod::PlanRepositoryMoves { root, regular_dir } => Body::RepositoryMoves(
            manager
                .repositories
                .plan_moves(&**manager, root.as_deref(), Layout::migration(regular_dir))
                .await?,
        ),
        RepositoryMethod::MoveRepositories { regular_dir } => Body::MovedRepositories(
            manager
                .repositories
                .move_all(&**manager, Layout::migration(regular_dir))
                .await?,
        ),
        RepositoryMethod::SyncRepository { repository } => Body::SyncedRepository(
            manager
                .repositories
                .sync_repository(&**manager, &repository)
                .await?,
        ),
        RepositoryMethod::FailingSyncs => {
            let failing = manager.repositories.failing_syncs().await;
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
                .agents
                .keep_repository_agent_environment(&**manager, &repository, environment)
                .await?;
            Body::Ok
        }
    }))
}

/// How a scoped caller is refused a repository not its own.
const OWN: &str = "scoped processes administer only their own repository";

/// Refuse a scoped caller's request for the repository on another joined
/// machine than its own, which `machine` names, where its repository is
/// registered; one that names this machine names none.
async fn ensure_own_machine(manager: &Manager, machine: &mut Option<String>) -> Result<()> {
    let Some(named) = machine.as_deref() else {
        return Ok(());
    };
    let named = manager.hub.named_machine(manager, named).await?;
    anyhow::ensure!(
        named.own,
        "scoped processes administer only their own repository, on their own machine"
    );
    *machine = None;
    Ok(())
}

/// The repository an administering `method` of `caller` acts in, which must
/// be the caller's own where the method names one, and where it names none,
/// the caller's own repository, if it has one; none for one that acts in
/// no single repository.
async fn administered_repository(
    manager: &Manager,
    caller: &Caller,
    method: &mut RepositoryMethod,
) -> Result<Option<RepositoryId>> {
    let own = async |named: &crate::repository::Selector| {
        let selected = manager.own_repository(caller, named, OWN).await?;
        Ok::<_, anyhow::Error>(manager.repositories.repository(&selected).await?.id)
    };
    Ok(match method {
        RepositoryMethod::RenameRepository { repository, .. }
        | RepositoryMethod::ShareRepository { repository, .. }
        | RepositoryMethod::LocalConfigPath { repository }
        | RepositoryMethod::EditLocalConfig { repository, .. }
        | RepositoryMethod::RemoveRepository { repository, .. }
        | RepositoryMethod::ListBranches { repository } => {
            let id = own(repository).await?;
            *repository = (&id).into();
            Some(id)
        }
        RepositoryMethod::KeepRepositoryAgentEnvironment {
            repository: repository_id,
            ..
        } => Some(own(&(&*repository_id).into()).await?),
        RepositoryMethod::KeptBranches { repository } => {
            let named = match repository.as_deref() {
                Some(named) => Some(own(&named.into()).await?),
                None => manager.caller_repository(caller).await?,
            };
            *repository = named.as_ref().map(ToString::to_string);
            named
        }
        RepositoryMethod::DeleteKeptBranches { branches } => {
            let mut repositories = Vec::new();
            for branch in branches.iter() {
                repositories.push(own(&(&branch.repository_id).into()).await?);
            }
            repositories.dedup();
            match repositories.as_slice() {
                [one] => Some(one.clone()),
                _ => None,
            }
        }
        _ => None,
    })
}

impl Checked for RepositoryMethod {
    fn target_check(&self) -> Option<TargetCheck> {
        match self {
            Self::ListRepositories
            | Self::RepositoriesElsewhere
            | Self::RegisterRepository { .. }
            | Self::RegisterContribution { .. }
            | Self::HoldContribution { .. }
            | Self::RenameRepository { .. }
            | Self::ShareRepository { .. }
            | Self::LocalConfigPath { .. }
            | Self::EditLocalConfig { .. }
            | Self::RemoveRepository { .. }
            | Self::PreviewRepositoryRemoval { .. }
            | Self::PlanRepositoryMoves { .. }
            | Self::MoveRepositories { .. }
            | Self::SyncRepository { .. }
            | Self::FailingSyncs
            | Self::ListBranches { .. }
            | Self::KeptBranches { .. }
            | Self::DeleteKeptBranches { .. }
            | Self::KeepRepositoryAgentEnvironment { .. }
            | Self::ReceiveCommits { .. } => None,
        }
    }
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
            RepositoryMethod::ListRepositories | RepositoryMethod::RepositoriesElsewhere => {
                return Ok(None);
            }
            // A start's commits, which another machine carries for a start
            // there that it confined to the caller's scope; they go under a
            // ref of Shoal's own and change no branch.
            RepositoryMethod::ReceiveCommits { .. } => {
                anyhow::ensure!(
                    caller.vouched.is_some(),
                    "only a start from another joined machine carries commits here"
                );
                return Ok(None);
            }
            // Nothing binds it to the registration it resumes, so a scoped
            // caller holds only where it administers without approval; one
            // that waited for a decision would leave the contribution unheld.
            RepositoryMethod::HoldContribution { repository } => {
                let key = PermissionKey::Admin;
                manager
                    .refuse_undirected(caller, key, Some(repository))
                    .await?;
                return Ok(None);
            }
            // The inbox lists them with the user's notifications.
            RepositoryMethod::FailingSyncs => {
                let action = Action::new(PermissionKey::Notifications, "the failing syncs");
                manager.authorize_action(Some(caller), action).await?;
                return Ok(None);
            }
            // It only reads what a removal of the caller's own repository
            // would remove.
            // It reads every space of the repository, as removing it takes
            // them all, so only a caller that removes its repository without
            // approval reads it; nothing binds an approval to a preview.
            RepositoryMethod::PreviewRepositoryRemoval {
                repository,
                machine,
            } => {
                ensure_own_machine(manager, machine).await?;
                *repository = manager.own_repository(caller, repository, OWN).await?;
                let id = manager.repositories.repository(&*repository).await?.id;
                (manager.refuse_undirected(caller, PermissionKey::Admin, Some(&id))).await?;
                return Ok(None);
            }
            RepositoryMethod::RemoveRepository { machine, .. } => {
                ensure_own_machine(manager, machine).await?;
                administered_repository(manager, caller, self).await?
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
            _ => administered_repository(manager, caller, self).await?,
        };
        // An edit that would give more than the caller holds is refused
        // before it asks for approval.
        if let RepositoryMethod::EditLocalConfig { changes, .. } = self {
            let held = manager.caller_permissions(caller, repository.as_ref());
            crate::config::edit::ensure_permissions_held(changes, &held.await?)?;
        }
        manager.administer(caller, self, repository).await?;
        Ok(None)
    }
}

#[cfg(test)]
mod tests;
