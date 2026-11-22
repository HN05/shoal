//! Removing a registered repository on this machine or on another joined
//! machine, where the hub carries the request.
use anyhow::{Context as _, Result};
use shoal_protocol::{
    hub::Member,
    model::RepositoryRemoval,
    protocol::{Body, Operation, RepositoryMethod},
    repository::Selector,
};

use super::{hub::Named, manager::Manager, scope::Caller};

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
        if let Some(machine) = self.other_machine(machine).await? {
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

    /// The other joined machine `named` names; none when it names none, or
    /// this one.
    async fn other_machine(&self, named: Option<&str>) -> Result<Option<Member>> {
        let Some(named) = named else {
            return Ok(None);
        };
        let Named { machine, own } = self.hub.named_machine(self, named).await?;
        Ok((!own).then_some(machine))
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
}
