//! Taking over a space whose machine is offline from the package the hub
//! keeps of it. See `docs/design/hub.md`.
use crate::Spaces;
use crate::daemon::Daemon;
use anyhow::Result;
use shoal_daemon_base::log::elapsed;
use shoal_ids::space_ids::SpaceId;
use shoal_protocol::{
    hub::packages::KeptPackage, protocol::RemovalMethod, space_names::SpaceSelector,
};

use super::PackageInput;
use crate::{
    model::{RemovedSpace, RestoredSpace},
    time::UnixTime,
    transfer::{Claim, PackageImport, PackageSource},
    {log, scope::Caller},
};

pub(super) struct Takeover {
    id: SpaceId,
    machine: String,
    kept: KeptPackage,
    /// Since when that machine had changes the package lacks, as it last
    /// reported.
    unsent_since: Option<UnixTime>,
}

impl Spaces {
    /// None for a space this machine has a record of, one whose machine is
    /// connected, one no package keeps, and while the hub cannot be asked,
    /// as a restore goes on then.
    pub(super) async fn takeover(
        &self,
        daemon: &dyn Daemon,
        selector: &SpaceSelector,
    ) -> Result<Option<Takeover>> {
        if let Some(id) = selector.given_id()
            && self.removed_here(id).await?
        {
            return Ok(None);
        }
        match self.offline_package(daemon, selector).await {
            Ok(takeover) => Ok(takeover),
            Err(error) => {
                log!(
                    "hub: could not ask whether the hub keeps a package of space {selector}: {error:#}"
                );
                Ok(None)
            }
        }
    }

    /// As the hub answers.
    async fn offline_package(
        &self,
        daemon: &dyn Daemon,
        selector: &SpaceSelector,
    ) -> Result<Option<Takeover>> {
        let Some((machine, listed)) = daemon.machine_of(selector).await? else {
            return Ok(None);
        };
        if machine.connected {
            return Ok(None);
        }
        let id = listed.space.id;
        let Some(kept) = daemon.kept_package(&id).await? else {
            return Ok(None);
        };
        Ok(Some(Takeover {
            id,
            machine: machine.name,
            kept,
            unsent_since: listed.space.unsent_since,
        }))
    }

    /// Here, or for a space whose machine is offline, as the hub's package of
    /// it records it.
    pub async fn restorable_space(
        &self,
        daemon: &dyn Daemon,
        selector: &SpaceSelector,
    ) -> Result<RemovedSpace> {
        let missing = match self.removed_space(selector).await {
            Ok(removed) => return Ok(removed),
            Err(missing) => missing,
        };
        match self.takeover(daemon, selector).await? {
            Some(takeover) => Ok(takeover.kept.record),
            None => Err(missing),
        }
    }

    /// Imports it under its ID for `caller`, warning of the changes its
    /// machine made that the package lacks.
    pub(super) async fn take_over_from_package(
        &self,
        daemon: &dyn Daemon,
        takeover: Takeover,
        caller: Option<&Caller>,
    ) -> Result<RestoredSpace> {
        let directory = tempfile::tempdir()?;
        let file = directory.path().join("package.shoal");
        daemon.fetch_package(&takeover.id, &file).await?;
        let import = PackageImport {
            package: PackageSource::File(file.clone()),
            repository: None,
            sessions: None,
            claim: Claim::Takeover,
        };
        let input = PackageInput::File(file);
        let imported = self.import_package(daemon, &import, input, caller).await?;
        let mut restored = imported.restored;
        let lacks = lacking(&takeover.machine, takeover.unsent_since, UnixTime::now());
        restored.warnings_mut().extend(lacks);
        Ok(restored)
    }

    /// Allowed only by the `admin` permission, since a takeover moves the
    /// space here as an import does.
    pub async fn ensure_takeover_allowed(
        &self,
        daemon: &dyn Daemon,
        caller: Option<&Caller>,
        selector: &SpaceSelector,
    ) -> Result<()> {
        let Some(caller) = caller else {
            return Ok(());
        };
        let request = RemovalMethod::RestoreSpace {
            space: selector.clone(),
        };
        daemon
            .coordination()
            .administer(daemon, caller, &request, None)
            .await
    }

    pub(crate) async fn removed_here(&self, id: &SpaceId) -> Result<bool> {
        let removed = self.removed_spaces().await?;
        Ok(removed.iter().any(|removed| &removed.space_id == id))
    }
}

/// Names how old the oldest change is at `now`. None for a package that has
/// them all.
pub fn lacking(machine: &str, unsent_since: Option<UnixTime>, now: UnixTime) -> Option<String> {
    let since = unsent_since?;
    Some(format!(
        "the package lacks changes machine {machine} made, the oldest {} ago; that work stays there, and once the machine returns it removes its copy and alerts you how to open that work",
        elapsed(now.duration_since(since)),
    ))
}

#[cfg(test)]
mod tests;
