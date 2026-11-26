//! Importing a package `shoal move` wrote, which the daemon reads itself.
//! Only once the whole package checks out does it record its spaces,
//! opening its own again as [`Spaces::import_space`] does and leaving those
//! it carries recorded for the caller to open after it, parents first.
use crate::Spaces;
use crate::daemon::Daemon;
use std::collections::{BTreeMap, BTreeSet};

use anyhow::{Result, ensure};

use super::imports::SpaceImport;
use crate::scope::Caller;
use crate::{
    model::RemovedSpace,
    refusal::{Reason, Refusal},
    transfer::{
        CarriedRecord, Claim, ImportedPackage, Manifest, ManifestExt, PackageImport, PackageSource,
        PackagedSpace, PackagedSpaceExt, Parts, Sessions,
    },
};
use shoal_ids::agent_session_ids::AgentSessionId;
use shoal_ids::move_tickets::MoveTicket;
use shoal_ids::space_ids::SpaceId;
use shoal_protocol::transcripts::CarriedSessionFile;

mod destinations;
mod input;

use destinations::Destinations;
pub use input::PackageInput;

/// Its spaces as recorded here, its own first, then those it carries,
/// parents first, with the warnings of reading it, while the directory its
/// parts are in lives.
struct Package {
    own: SpaceImport,
    carried: Vec<SpaceImport>,
    warnings: Vec<String>,
    _parts: input::Unpacked,
}

impl Spaces {
    /// Reads it whole and checks it, then records its spaces as removed here
    /// and opens its own again. A scoped `caller` imports only into its own
    /// repository and clones none. An error before anything is recorded
    /// says so.
    pub async fn import_package(
        &self,
        daemon: &dyn Daemon,
        import: &PackageImport,
        input: PackageInput,
        caller: Option<&Caller>,
    ) -> Result<ImportedPackage> {
        let package = (self.read_package(daemon, import, input, caller).await)
            .map_err(|error| Refusal::wrap(Reason::NotImported, error))?;
        let Package {
            own,
            carried,
            warnings,
            _parts,
        } = package;
        let original = own.space.space_id.clone();
        let mut restored = self.import_space(daemon, own).await?;
        restored.warnings_mut().extend(warnings);
        let opened = (original, restored.id().clone());
        let carried = self.record_carried(daemon, carried, opened).await;
        Ok(ImportedPackage { restored, carried })
    }

    /// For a removed space another joined machine moves here with the move
    /// `ticket`. Opens none, and returns the IDs of the carried spaces it
    /// recorded. An error before its own space is recorded says so.
    pub async fn import_removed(
        &self,
        daemon: &dyn Daemon,
        input: PackageInput,
        ticket: MoveTicket,
    ) -> Result<Vec<SpaceId>> {
        let import = PackageImport {
            package: PackageSource::Stream,
            repository: None,
            sessions: None,
            claim: Claim::Move(ticket),
        };
        let package = (self.read_package(daemon, &import, input, None).await)
            .map_err(|error| Refusal::wrap(Reason::NotImported, error))?;
        let Package {
            own,
            carried,
            _parts,
            ..
        } = package;
        let (removed, _) = self.record_import(daemon, own).await?;
        let opened = (removed.space_id.clone(), removed.space_id);
        let recorded = self.record_carried(daemon, carried, opened).await;
        Ok((recorded.into_iter())
            .filter_map(|record| match record {
                CarriedRecord::Recorded { space, .. } => Some(space),
                CarriedRecord::Failed { .. } => None,
            })
            .collect())
    }

    /// Refuses what any of its spaces would refuse before one is recorded.
    async fn read_package(
        &self,
        daemon: &dyn Daemon,
        import: &PackageImport,
        input: PackageInput,
        caller: Option<&Caller>,
    ) -> Result<Package> {
        let (manifest, input) = input.read_manifest().await?;
        let warnings = copy_warnings(&manifest, import.sessions)?;
        let destinations = self
            .destinations(daemon, &manifest, import.repository.as_ref(), caller)
            .await?;
        let directory = self.unpacking_directory(daemon, destinations.own()).await?;
        let parts = input.read_parts(&manifest, directory).await?;
        self.ensure_none_here(&manifest).await?;
        let kept = (import.sessions, import.claim.clone());
        let (own, carried) = space_imports(&manifest, kept, &destinations, &parts.parts);
        Ok(Package {
            own,
            carried,
            warnings,
            _parts: parts,
        })
    }

    /// In order, after the package's own space, which `opened` maps from its
    /// ID in the package to the one it opened under. Each goes below the
    /// copy this import made of the space it was below. Goes on after one
    /// fails.
    async fn record_carried(
        &self,
        daemon: &dyn Daemon,
        carried: Vec<SpaceImport>,
        opened: (SpaceId, SpaceId),
    ) -> Vec<CarriedRecord> {
        let mut before = Before::new(opened);
        let mut records = Vec::new();
        for mut import in carried {
            let original = import.space.space_id.clone();
            let name = import.space.name.clone();
            before.place(&mut import);
            records.push(match self.record_import(daemon, import).await {
                Ok((removed, warnings)) => {
                    before.add(original, &removed.space_id);
                    let space = removed.space_id;
                    CarriedRecord::Recorded {
                        name,
                        space,
                        warnings,
                    }
                }
                Err(error) => CarriedRecord::Failed {
                    name,
                    error: format!("{error:#}"),
                },
            });
        }
        records
    }

    /// Refuses while a space here has the ID one of its spaces keeps.
    async fn ensure_none_here(&self, manifest: &Manifest) -> Result<()> {
        let copy = manifest.copy.is_some();
        for packaged in manifest.packaged() {
            self.ensure_not_here(&packaged.record.space_id, copy)
                .await?;
        }
        Ok(())
    }
}

/// The IDs they have here, and those by their IDs in the package.
struct Before {
    copies: BTreeMap<SpaceId, SpaceId>,
    recorded: BTreeSet<SpaceId>,
}

impl Before {
    /// `opened` maps the package's own space from its ID in the package to
    /// the one it opened under.
    fn new(opened: (SpaceId, SpaceId)) -> Self {
        Self {
            recorded: BTreeSet::from([opened.1.clone()]),
            copies: BTreeMap::from([opened]),
        }
    }

    /// Places `import` after them, below the copies a copy was below.
    fn place(&self, import: &mut SpaceImport) {
        if import.copy {
            import.copies = self.copies.clone();
        }
        import.recorded = self.recorded.clone();
    }

    fn add(&mut self, original: SpaceId, space: &SpaceId) {
        self.copies.insert(original, space.clone());
        self.recorded.insert(space.clone());
    }
}

/// Refuses it when `sessions` keeps the sessions its agents start forks of.
pub fn copy_warnings(manifest: &Manifest, sessions: Option<bool>) -> Result<Vec<String>> {
    let Some(copied) = manifest.copy else {
        return Ok(Vec::new());
    };
    ensure!(
        sessions != Some(true),
        "the export holds a copy, whose agents start from forks of the original's sessions; \
         --keep-sessions would continue those sessions"
    );
    Ok(copied
        .earlier_files
        .then(|| {
            "the agents' conversations go past the copied checkpoint and describe work its files \
             do not contain"
                .to_owned()
        })
        .into_iter()
        .collect())
}

/// Its own and those it carries. Their agents keep their sessions on this
/// machine as `sessions` says. Each is claimed at the hub as `claim` says,
/// unless they are copies.
fn space_imports(
    manifest: &Manifest,
    (sessions, claim): (Option<bool>, Claim),
    destinations: &Destinations,
    parts: &Parts,
) -> (SpaceImport, Vec<SpaceImport>) {
    let machine = crate::process::machine::id();
    let machine = machine.as_deref();
    // A copy takes IDs of its own, which no other machine has.
    let claim = match manifest.copy {
        Some(_) => Claim::Plain,
        None => claim,
    };
    let import_of = |packaged: &PackagedSpace, kept: Sessions| {
        space_import(
            manifest,
            destinations,
            parts,
            packaged,
            (kept, claim.clone()),
        )
    };
    let own = import_of(&manifest.space, manifest.sessions_on(machine, sessions));
    let carried = (manifest.carried.iter())
        .map(|carried| {
            let kept = manifest.carried_sessions_on(carried, machine, sessions);
            import_of(&carried.space, kept)
        })
        .collect();
    (own, carried)
}

fn space_import(
    manifest: &Manifest,
    destinations: &Destinations,
    parts: &Parts,
    packaged: &PackagedSpace,
    (sessions, claim): (Sessions, Claim),
) -> SpaceImport {
    let space = packaged.record_with(sessions);
    let id = &space.space_id;
    SpaceImport {
        repository: destinations.of(&space),
        bundle: (parts.bundles.get(id)).map(|bundle| bundle.path().to_owned()),
        prerequisite: packaged.prerequisite.clone(),
        session_files: session_files(parts, &space),
        space,
        copy: manifest.copy.is_some(),
        copies: BTreeMap::new(),
        recorded: BTreeSet::new(),
        claim,
    }
}

fn session_files(parts: &Parts, space: &RemovedSpace) -> Vec<CarriedSessionFile> {
    let stopped = space.agents.iter().map(|agent| &agent.record.session);
    let last = space.last_sessions.iter().map(|last| &last.session);
    let kept: BTreeSet<&AgentSessionId> = stopped.chain(last).flatten().collect();
    (parts.sessions.iter())
        .filter(|imported| kept.contains(&imported.file.session))
        .map(|imported| imported.carried())
        .collect()
}

#[cfg(test)]
mod tests;
