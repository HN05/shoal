//! Removing a registered repository on this machine or on another joined
//! machine, where the hub carries the request, and what that removal would
//! remove there.
use anyhow::{Context as _, Result};
use futures_util::{StreamExt as _, stream};
use shoal_core::removal::InspectionPolicy;
use shoal_protocol::{
    hub::Member,
    model::{RepositoryRemoval, RepositoryRemovalPreview, Space, SpaceWork, WorktreeWork},
    protocol::{Body, Operation, RepositoryMethod},
    repository::Selector,
    space_names::SpaceSelector,
};

use super::{manager::Manager, scope::Caller};

/// How many workspaces a removal's preview inspects at once.
const PREVIEW_CONCURRENCY: usize = 4;

impl Manager {
    /// Remove the repository `selector` names for `caller` on the joined
    /// machine `machine` names, or on this one, where a shared one stops
    /// being shared on every machine at once.
    pub(super) async fn remove_repository(
        &self,
        selector: Selector,
        machine: Option<&str>,
        caller: Option<&Caller>,
    ) -> Result<RepositoryRemoval> {
        if let Some(machine) = self.hub.other_machine(self, machine).await? {
            let method = RepositoryMethod::RemoveRepository {
                repository: selector,
                machine: None,
            };
            let body = (self.ask_other_machine(&machine, method, caller).await)
                .with_context(|| format!("remove the repository on machine {}", machine.name))?;
            return RepositoryRemoval::try_from(body);
        }
        let shared = (self.repositories.repository(selector.clone()).await?)
            .shared_url
            .is_some();
        let removal = self.repositories.remove(self, selector).await?;
        if shared {
            self.hub.shared_repositories_changed();
        }
        Ok(removal)
    }

    /// What removing the repository `selector` names would remove, for
    /// `caller`, on the joined machine `machine` names, or on this one.
    pub(super) async fn preview_repository_removal(
        &self,
        selector: Selector,
        machine: Option<&str>,
        caller: Option<&Caller>,
    ) -> Result<RepositoryRemovalPreview> {
        if let Some(machine) = self.hub.other_machine(self, machine).await? {
            let method = RepositoryMethod::PreviewRepositoryRemoval {
                repository: selector,
                machine: None,
            };
            let body = self.ask_other_machine(&machine, method, caller).await?;
            return RepositoryRemovalPreview::try_from(body);
        }
        let repository = self.repositories.repository(selector).await?;
        let of_repository = (self.spaces.present_spaces(self).await?.into_iter())
            .filter(|space| space.repository_id.as_ref() == Some(&repository.id));
        let spaces = (stream::iter(of_repository))
            .map(|space| self.space_work(space))
            .buffered(PREVIEW_CONCURRENCY)
            .collect()
            .await;
        Ok(RepositoryRemovalPreview { repository, spaces })
    }

    /// What `machine` answers `method` from `caller` with, through the hub;
    /// it fails at once, naming the machine, while that machine is offline.
    async fn ask_other_machine(
        &self,
        machine: &Member,
        method: RepositoryMethod,
        caller: Option<&Caller>,
    ) -> Result<Body> {
        let operation = Operation::Repository(method);
        (self.hub.ask_machine(self, machine, operation, caller)).await
    }

    /// `space` with what of its own its removal would lose: none for a
    /// chat, or a worktree that cannot be read now.
    async fn space_work(&self, space: Space) -> SpaceWork {
        let work = match space.kind().has_worktree() {
            true => {
                let selector = SpaceSelector::id(&space.id);
                let inspected =
                    self.spaces
                        .check_removal(self, &selector, InspectionPolicy::GitOnly);
                (inspected.await.ok()).map(|inspection| WorktreeWork {
                    dirty: inspection.check.dirty,
                    unpushed_commits: inspection.check.unpushed_commits,
                })
            }
            false => None,
        };
        SpaceWork {
            space: space.space_ref(),
            number: space.number,
            work,
        }
    }
}

#[cfg(test)]
mod tests;
