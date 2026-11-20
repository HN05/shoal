//! Importing a package `shoal move` wrote, which the daemon reads itself:
//! its manifest first, then, once the repositories its spaces go to are
//! here, cloning those it lacks from their origins, its parts into the
//! directory of the repository it goes to. Only once the whole package
//! checks out does it record its spaces, opening its own again as
//! [`Manager::import_space`] does and leaving those it carries recorded for
//! the caller to open after it, parents first.
use std::collections::{BTreeMap, BTreeSet};

use anyhow::{Result, ensure};

use super::imports::SpaceImport;
use crate::daemon::{manager::Manager, scope::Caller};
use crate::{
    model::RemovedSpace,
    refusal::{Reason, Refusal},
    transfer::{
        CarriedRecord, ImportedPackage, Manifest, ManifestExt, PackageImport, PackagedSpace,
        PackagedSpaceExt, Parts, Sessions,
    },
};
use shoal_ids::agent_session_ids::AgentSessionId;
use shoal_ids::space_ids::SpaceId;
use shoal_protocol::transcripts::CarriedSessionFile;

mod destinations;
mod input;

use destinations::Destinations;
pub(crate) use input::PackageInput;

#[cfg(test)]
mod tests;

/// A package read and checked whole: its spaces as they are recorded here,
/// its own first, then those it carries, parents first, with the warnings
/// of reading it, while the directory its parts are in lives.
struct Package {
    own: SpaceImport,
    carried: Vec<SpaceImport>,
    warnings: Vec<String>,
    _parts: input::Unpacked,
}

impl Manager {
    /// Import the package `input` holds as `import` says: read it whole and
    /// check it, then record its spaces as removed here and open its own
    /// again. `caller`, when scoped, imports only into its own repository
    /// and clones none. An error before anything is recorded says so.
    pub(crate) async fn import_package(
        &self,
        import: &PackageImport,
        input: PackageInput,
        caller: Option<&Caller>,
    ) -> Result<ImportedPackage> {
        let package = (self.read_package(import, input, caller).await)
            .map_err(|error| Refusal::wrap(Reason::NotImported, error))?;
        let Package {
            own,
            carried,
            warnings,
            _parts,
        } = package;
        let original = own.space.space_id.clone();
        let mut restored = self.import_space(own).await?;
        restored.warnings_mut().extend(warnings);
        let opened = (original, restored.id().clone());
        let carried = self.record_carried(carried, opened).await;
        Ok(ImportedPackage { restored, carried })
    }

    /// Read the package `input` holds as `import` says, for `caller`, into
    /// the spaces it records, refusing what any of them would refuse before
    /// one is recorded.
    async fn read_package(
        &self,
        import: &PackageImport,
        input: PackageInput,
        caller: Option<&Caller>,
    ) -> Result<Package> {
        let (manifest, input) = input.read_manifest().await?;
        let warnings = copy_warnings(&manifest, import.sessions)?;
        let destinations = self
            .destinations(&manifest, import.repository.as_ref(), caller)
            .await?;
        let directory = self.unpacking_directory(destinations.own()).await?;
        let parts = input.read_parts(&manifest, directory).await?;
        self.ensure_none_here(&manifest).await?;
        let (own, carried) = space_imports(&manifest, import.sessions, &destinations, &parts.parts);
        Ok(Package {
            own,
            carried,
            warnings,
            _parts: parts,
        })
    }

    /// Record each of `carried` as removed here, in order, after the
    /// package's own space, `opened` from its ID in the package to the one
    /// it opened under, which its caller sets up before it opens them:
    /// each below the copy this import made of the space it was below,
    /// going on after one fails.
    async fn record_carried(
        &self,
        carried: Vec<SpaceImport>,
        opened: (SpaceId, SpaceId),
    ) -> Vec<CarriedRecord> {
        let mut before = Before::new(opened);
        let mut records = Vec::new();
        for mut import in carried {
            let original = import.space.space_id.clone();
            let name = import.space.name.clone();
            before.place(&mut import);
            records.push(match self.record_import(import).await {
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

    /// Refuse the package `manifest` while a space here has the ID one of
    /// its spaces keeps.
    async fn ensure_none_here(&self, manifest: &Manifest) -> Result<()> {
        let copy = manifest.copy.is_some();
        for packaged in manifest.packaged() {
            self.ensure_not_here(&packaged.record.space_id, copy)
                .await?;
        }
        Ok(())
    }
}

/// The spaces of a package an import recorded or opened before the next
/// one: the IDs they have here, and those by their IDs in the package.
struct Before {
    copies: BTreeMap<SpaceId, SpaceId>,
    recorded: BTreeSet<SpaceId>,
}

impl Before {
    /// The package's own space, `opened` from its ID in the package to the
    /// one it opened under.
    fn new(opened: (SpaceId, SpaceId)) -> Self {
        Self {
            recorded: BTreeSet::from([opened.1.clone()]),
            copies: BTreeMap::from([opened]),
        }
    }

    /// Place `import` after them, below the copies a copy was below.
    fn place(&self, import: &mut SpaceImport) {
        if import.copy {
            import.copies = self.copies.clone();
        }
        import.recorded = self.recorded.clone();
    }

    /// Add the space `original` the import recorded as `space`.
    fn add(&mut self, original: SpaceId, space: &SpaceId) {
        self.copies.insert(original, space.clone());
        self.recorded.insert(space.clone());
    }
}

/// The warnings of importing the copy `manifest` holds, refusing it when
/// `sessions` keeps the sessions its agents start forks of.
fn copy_warnings(manifest: &Manifest, sessions: Option<bool>) -> Result<Vec<String>> {
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

/// The imports of the spaces of `manifest`, its own and those it carries,
/// whose agents keep their sessions on this machine as `sessions` says,
/// into the repositories `destinations` names, with the bundles and session
/// files `parts` hold for them.
fn space_imports(
    manifest: &Manifest,
    sessions: Option<bool>,
    destinations: &Destinations,
    parts: &Parts,
) -> (SpaceImport, Vec<SpaceImport>) {
    let machine = crate::process::machine::id();
    let machine = machine.as_deref();
    let import_of = |packaged: &PackagedSpace, kept: Sessions| {
        space_import(manifest, destinations, parts, packaged, kept)
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

/// The import of the space `packaged` of `manifest`, whose agents keep
/// `sessions`, into the repository `destinations` names for it, with the
/// bundle and session files `parts` hold for it.
fn space_import(
    manifest: &Manifest,
    destinations: &Destinations,
    parts: &Parts,
    packaged: &PackagedSpace,
    sessions: Sessions,
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
    }
}

/// The session files among `parts` of the sessions the agents of `space`
/// continue.
fn session_files(parts: &Parts, space: &RemovedSpace) -> Vec<CarriedSessionFile> {
    let stopped = space.agents.iter().map(|agent| &agent.record.session);
    let last = space.last_sessions.iter().map(|last| &last.session);
    let kept: BTreeSet<&AgentSessionId> = stopped.chain(last).flatten().collect();
    (parts.sessions.iter())
        .filter(|imported| kept.contains(&imported.file.session))
        .map(|imported| imported.carried())
        .collect()
}
