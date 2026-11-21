//! Packages of spaces for `shoal move`: the daemon stops a space's agents
//! for a move that ends it once its destination has the package, copies
//! one that keeps running, or writes a removed one's record, each with the
//! spaces below it that the package carries, and writes what continues
//! them on another machine; and the hub's packages of an intermittent
//! machine's spaces, which keep running. Each runs one path for a space of
//! either kind; what a workspace's worktree adds is its worktree step.
use crate::transfer::ManifestExt;
use anyhow::{Context, Result};
use shoal_ids::checkpoint_numbers::CheckpointNumber;
use shoal_ids::space_ids::SpaceId;
use std::collections::BTreeMap;

use crate::{
    config::Effective,
    daemon::manager::Manager,
    model::{RemovedSpace, Repository, Space},
    removal::{InspectionPolicy, RemovalResult, UnsavedWork},
    transfer::{Carrying, ExportedSpace, Manifest, MoveDestination, SessionSource},
};
use shoal_protocol::space_names::SpaceSelector;
use shoal_protocol::transcripts::Environments;

mod carried;
mod copy;
mod kept;
mod moving;
pub(crate) use moving::MovePacking;
mod records;
mod retiring;
mod staying;
mod worktree;

/// A space prepared for its package, moving, removed or copied, with the
/// spaces it carries: the repositories they belong to, the package without
/// its session files, the settings describing their agents, the session
/// files an import kept for them, and what the package leaves out.
struct Prepared {
    repos: Vec<Repository>,
    manifest: Manifest,
    settings: Vec<Result<Effective>>,
    kept_sessions: Vec<SessionSource>,
    warnings: Vec<String>,
}

impl Prepared {
    /// The package of the space `record` records and the spaces `carried`
    /// records, whose agents `settings` describes, one for each in that
    /// order, with the session files an import kept for them.
    async fn of(
        manager: &Manager,
        record: RemovedSpace,
        carried: Vec<RemovedSpace>,
        settings: Vec<Result<Effective>>,
    ) -> Result<Self> {
        let records: Vec<&RemovedSpace> = std::iter::once(&record).chain(&carried).collect();
        let warnings = records.iter().copied().filter_map(no_session).collect();
        let kept_sessions = manager.kept_sessions_of(&records).await;
        let repos = manager.repositories_of(&records).await?;
        let manifest = manager.manifest_of(&repos, record, carried).await?;
        Ok(Self {
            repos,
            manifest,
            settings,
            kept_sessions,
            warnings,
        })
    }
}

impl Manager {
    /// Write the removal record of the space with the ID `selector` gives,
    /// which stays, with the records of the spaces its removal took that
    /// `carrying` carries, for another Shoal to the new file `file`, once
    /// the daemon can write there.
    pub async fn export_removed_space(
        &self,
        selector: &SpaceSelector,
        file: std::path::PathBuf,
        carrying: &Carrying,
    ) -> Result<ExportedSpace> {
        shoal_daemon_base::writable::ensure_writable(crate::transfer::directory(&file)).await?;
        let prepared = self.prepare_kept_record(selector, carrying).await?;
        self.write_prepared(prepared, file).await
    }

    /// Prepare the package of the removal record of the space `selector`
    /// names by ID, which stays, with the records of the spaces its removal
    /// took that `carrying` carries.
    async fn prepare_kept_record(
        &self,
        selector: &SpaceSelector,
        carrying: &Carrying,
    ) -> Result<Prepared> {
        let records = self.removed_spaces().await?;
        let removed = (records.iter())
            .find(|removed| selector.given_id() == Some(&removed.space_id))
            .with_context(|| format!("no space or restorable space {selector}"))?
            .clone();
        let carried = carried::taken_by_removal(&records, &removed.space_id, carrying);
        let mut settings = Vec::new();
        for record in std::iter::once(&removed).chain(&carried) {
            settings.push(self.space_settings_in(record.repository_id.as_ref()).await);
        }
        self.prepare_removed(removed, carried, settings).await
    }

    /// Prepare the package of the space `removed` records with the spaces
    /// `carried` records, as their removal kept them, whose agents
    /// `settings` describes, one for each.
    async fn prepare_removed(
        &self,
        removed: RemovedSpace,
        carried: Vec<RemovedSpace>,
        settings: Vec<Result<Effective>>,
    ) -> Result<Prepared> {
        for record in std::iter::once(&removed).chain(&carried) {
            worktree::ensure_files_kept(record)?;
        }
        Prepared::of(self, removed, carried, settings).await
    }

    /// Leave the spaces below `space` that a selection does not carry here,
    /// as [`Self::leave_behind`] does, or for a move to a joined machine
    /// below the spaces they are below, detached until the removal here
    /// ends, when [`Self::attach_behind`] links them again; carrying every
    /// space below it leaves none, and one that joins it meanwhile retains
    /// it.
    async fn leave_uncarried(
        &self,
        space: &Space,
        carried: &[Space],
        (carrying, destination): (&Carrying, &MoveDestination),
    ) -> Result<()> {
        match (carrying, destination) {
            (Carrying::Subtree, _) => Ok(()),
            (Carrying::Selected(_), MoveDestination::Machine(_)) => {
                self.detach_behind(space, carried).await
            }
            (Carrying::Selected(_), _) => self.leave_behind(space, carried).await,
        }
    }

    /// Remove `space` for its package through the removal path, deleting
    /// the `unsaved` work the caller confirmed and taking only the spaces
    /// `carried` with it among the spaces below that it confirms: a space
    /// that joins it meanwhile retains it. For a move another machine
    /// imported, `moved` holds each workspace's checkpoint in its package,
    /// from which none may differ once its removal hooks ran.
    async fn remove_packaged(
        &self,
        space: &Space,
        carried: &[Space],
        unsaved: UnsavedWork,
        inspection: InspectionPolicy,
        moved: Option<&BTreeMap<SpaceId, CheckpointNumber>>,
    ) -> Result<RemovalResult> {
        let confirmed = |id: &&SpaceId| unsaved.children().is_none_or(|ids| ids.contains(id));
        let carried = carried.iter().map(|space| &space.id);
        let taking: Vec<SpaceId> = carried.filter(confirmed).cloned().collect();
        let selector = SpaceSelector::id(&space.id);
        (self.remove_selected_taking(&selector, unsaved, inspection, &taking, moved)).await
    }

    /// The repositories the spaces `records` belong to, each once.
    async fn repositories_of(&self, records: &[&RemovedSpace]) -> Result<Vec<Repository>> {
        let mut repos: Vec<Repository> = Vec::new();
        for id in records
            .iter()
            .filter_map(|record| record.repository_id.as_ref())
        {
            if !repos.iter().any(|repo| &repo.id == id) {
                repos.push(self.repositories.repository(id).await?);
            }
        }
        Ok(repos)
    }

    /// The session files an import kept for the spaces `records` records.
    async fn kept_sessions_of(&self, records: &[&RemovedSpace]) -> Vec<SessionSource> {
        let mut kept = Vec::new();
        for record in records {
            kept.extend(self.kept_session_sources(record).await);
        }
        kept
    }

    /// Write the package of `prepared` to the new file `file`.
    async fn write_prepared(
        &self,
        prepared: Prepared,
        file: std::path::PathBuf,
    ) -> Result<ExportedSpace> {
        let Prepared {
            repos,
            manifest,
            settings,
            kept_sessions,
            mut warnings,
        } = prepared;
        let (manifest, left_out) = self
            .write_package(&repos, manifest, settings, kept_sessions, file)
            .await?;
        warnings.extend(left_out);
        Ok(ExportedSpace { manifest, warnings })
    }

    /// Write `manifest`'s package, spaces of `repos` or of none, whose
    /// agents `settings` describes, one for each space it packages, with
    /// their session files to the new file `file`, those of a session in
    /// `kept` from there, and the warnings for what it left out.
    async fn write_package(
        &self,
        repos: &[Repository],
        manifest: Manifest,
        settings: Vec<Result<Effective>>,
        kept: Vec<SessionSource>,
        file: std::path::PathBuf,
    ) -> Result<(Manifest, Vec<String>)> {
        let (mut sessions, mut warnings) = self.session_sources(repos, settings, &manifest).await;
        sessions.retain(|found| !kept.iter().any(|kept| kept.session == found.session));
        sessions.extend(kept);
        let (manifest, left_out) = crate::transfer::export(repos, manifest, sessions, file).await?;
        warnings.extend(left_out);
        Ok((manifest, warnings))
    }

    /// The files of the sessions the agents of the package `manifest`, of
    /// `repos` or of none, reported, as `settings` describes the agents of
    /// each space it packages, wherever they ran, with a warning for each
    /// session left out, which then starts over on the importing machine.
    async fn session_sources(
        &self,
        repos: &[Repository],
        settings: Vec<Result<Effective>>,
        manifest: &Manifest,
    ) -> (Vec<SessionSource>, Vec<String>) {
        let (environments, mut warnings) = self.package_environments(repos).await;
        let environments = std::sync::Arc::new(environments);
        let mut sources = Vec::new();
        for (sessions, settings) in manifest.each_space_sessions().zip(settings) {
            let settings = match settings {
                Ok(settings) => settings,
                Err(error) => {
                    warnings.push(format!(
                        "cannot export the agents' session files: {error:#}"
                    ));
                    continue;
                }
            };
            let environments = environments.clone();
            let found = tokio::task::spawn_blocking(move || {
                crate::transfer::session_sources(&settings, &environments, &sessions)
            })
            .await;
            match found {
                Ok((found, more)) => {
                    sources.extend(found);
                    warnings.extend(more);
                }
                Err(error) => {
                    warnings.push(format!("cannot export the agents' session files: {error}"))
                }
            }
        }
        (sources, warnings)
    }

    /// The environments the agents of `repos` ran in, where their session
    /// files are, with a warning when one cannot be read.
    async fn package_environments(&self, repos: &[Repository]) -> (Environments, Vec<String>) {
        let mut warnings = Vec::new();
        // Only a repository records the environments its agents ran in.
        let mut environments = Vec::new();
        for repo in repos {
            match self.agent_environments(&repo.id).await {
                Ok(recorded) => environments.extend(recorded),
                Err(error) => warnings.push(format!(
                    "agents' session files are looked up in the default places only: {error:#}"
                )),
            }
        }
        (Environments::new(&self.paths.home, &environments), warnings)
    }
}

/// The warning for a package of the space `record` records whose agent
/// saved no session to continue, which starts as a new one where it is
/// imported.
fn no_session(record: &RemovedSpace) -> Option<String> {
    record.saved_no_session().then(|| {
        format!(
            "{} saved no session of its agent, which starts as a new one where it is imported",
            record.name
        )
    })
}

#[cfg(test)]
mod tests;
