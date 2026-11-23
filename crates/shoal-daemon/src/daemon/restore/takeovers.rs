//! Taking over a space whose machine is offline from the package the hub
//! keeps of it: `shoal restore` of the space fetches the package and imports
//! it under the space's ID, as `shoal restore --from` does, whose claim at
//! the hub makes the space this machine's.
use anyhow::Result;
use shoal_daemon_base::log::elapsed;
use shoal_ids::space_ids::SpaceId;
use shoal_protocol::{
    hub::packages::KeptPackage, protocol::RemovalMethod, space_names::SpaceSelector,
};

use super::PackageInput;
use crate::{
    daemon::{log, manager::Manager, scope::Caller},
    model::{RemovedSpace, RestoredSpace},
    time::UnixTime,
    transfer::{Claim, PackageImport, PackageSource},
};

/// A space of an offline machine that a restore takes over from the hub's
/// package of it.
pub(super) struct Takeover {
    id: SpaceId,
    /// The name of the machine it runs on.
    machine: String,
    kept: KeptPackage,
    /// Since when that machine had changes of it the package lacks, as it
    /// last reported.
    unsent_since: Option<UnixTime>,
}

impl Manager {
    /// The space `selector` names on another joined machine that is
    /// offline, with the package the hub keeps of it; none for a space this
    /// machine has a record of, one whose machine is connected, one no
    /// package keeps, and while the hub cannot be asked, as a restore goes
    /// on then.
    pub(super) async fn takeover(&self, selector: &SpaceSelector) -> Result<Option<Takeover>> {
        if let Some(id) = selector.given_id()
            && self.removed_here(id).await?
        {
            return Ok(None);
        }
        match self.offline_package(selector).await {
            Ok(takeover) => Ok(takeover),
            Err(error) => {
                log!(
                    "hub: could not ask whether the hub keeps a package of space {selector}: {error:#}"
                );
                Ok(None)
            }
        }
    }

    /// The space `selector` names on another joined machine that is
    /// offline, with the package the hub keeps of it, as the hub answers.
    async fn offline_package(&self, selector: &SpaceSelector) -> Result<Option<Takeover>> {
        let Some((machine, listed)) = self.hub.machine_of(self, selector).await? else {
            return Ok(None);
        };
        if machine.connected {
            return Ok(None);
        }
        let id = listed.space.id;
        let Some(kept) = self.hub.kept_package(self, &id).await? else {
            return Ok(None);
        };
        Ok(Some(Takeover {
            id,
            machine: machine.name,
            kept,
            unsent_since: listed.space.unsent_since,
        }))
    }

    /// The removed space `selector` names, here or, for a space whose
    /// machine is offline, as the hub's package of it records it, which a
    /// restore takes over.
    pub(crate) async fn restorable_space(&self, selector: &SpaceSelector) -> Result<RemovedSpace> {
        let missing = match self.removed_space(selector).await {
            Ok(removed) => return Ok(removed),
            Err(missing) => missing,
        };
        match self.takeover(selector).await? {
            Some(takeover) => Ok(takeover.kept.record),
            None => Err(missing),
        }
    }

    /// Take over the space `takeover` names: fetch the hub's package of it
    /// and import it under its ID for `caller`, warning of the changes its
    /// machine made that the package lacks.
    pub(super) async fn take_over_from_package(
        &self,
        takeover: Takeover,
        caller: Option<&Caller>,
    ) -> Result<RestoredSpace> {
        let directory = tempfile::tempdir()?;
        let file = directory.path().join("package.shoal");
        self.hub.fetch_package(self, &takeover.id, &file).await?;
        let import = PackageImport {
            package: PackageSource::File(file.clone()),
            repository: None,
            sessions: None,
            claim: Claim::Takeover,
        };
        let input = PackageInput::File(file);
        let imported = self.import_package(&import, input, caller).await?;
        let mut restored = imported.restored;
        let lacks = lacking(&takeover.machine, takeover.unsent_since, UnixTime::now());
        restored.warnings_mut().extend(lacks);
        Ok(restored)
    }

    /// Refuse a scoped `caller` the takeover of the space `selector` names
    /// unless its `admin` permission allows it, since a takeover moves the
    /// space here as an import does.
    pub(super) async fn ensure_takeover_allowed(
        &self,
        caller: Option<&Caller>,
        selector: &SpaceSelector,
    ) -> Result<()> {
        let Some(caller) = caller else {
            return Ok(());
        };
        let request = RemovalMethod::RestoreSpace {
            space: selector.clone(),
        };
        self.coordination
            .administer(self, caller, &request, None)
            .await
    }

    /// Whether this machine has a removal record of the space `id`.
    async fn removed_here(&self, id: &SpaceId) -> Result<bool> {
        let removed = self.removed_spaces().await?;
        Ok(removed.iter().any(|removed| &removed.space_id == id))
    }
}

/// The warning that a package lacks the changes its machine, named
/// `machine`, made `unsent_since`, naming how old the oldest is at `now`;
/// none for a package that has them all.
fn lacking(machine: &str, unsent_since: Option<UnixTime>, now: UnixTime) -> Option<String> {
    let since = unsent_since?;
    Some(format!(
        "the package lacks changes machine {machine} made, the oldest {} ago; that work stays there, and once the machine returns it removes its copy and alerts you how to open that work",
        elapsed(now.duration_since(since)),
    ))
}

#[cfg(test)]
mod tests;
