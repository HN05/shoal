//! Packages of spaces for `shoal move`: the daemon removes a space here,
//! copies one that keeps running, or stops one's agents for a move that
//! ends it once imported, each with the spaces below it that the package
//! carries, and writes what continues them on another machine. Each runs
//! one path for a space of either kind; what a workspace's worktree adds is
//! its worktree step.
use crate::transfer::ManifestExt;
use anyhow::{Context, Result};
use shoal_ids::space_ids::SpaceId;

use crate::{
    config::Effective,
    daemon::workspace::Manager,
    model::{RemovedSpace, Repository, Space},
    removal::{InspectionPolicy, RemovalResult, UnsavedWork},
    transfer::{Carrying, ExportedSpace, Manifest, SessionSource},
};
use shoal_protocol::space_names::SpaceSelector;
use shoal_protocol::transcripts::Environments;

mod carried;
mod copy;
mod moving;
mod worktree;

/// A space prepared for its package, ended here or copied, with the
/// spaces it carries: the repositories they belong to, the package without
/// its session files, the removal that ended them, the settings describing
/// their agents, the session files an import kept for them, and what the
/// package leaves out.
struct Prepared {
    repos: Vec<Repository>,
    manifest: Manifest,
    removal: Option<RemovalResult>,
    settings: Result<Effective>,
    kept_sessions: Vec<SessionSource>,
    warnings: Vec<String>,
}

impl Prepared {
    /// The package of the space `record` records and the spaces `carried`
    /// records, whose agents `settings` describes, with the session files
    /// an import kept for them.
    async fn of(
        manager: &Manager,
        record: RemovedSpace,
        carried: Vec<RemovedSpace>,
        settings: Result<Effective>,
    ) -> Result<Self> {
        let records: Vec<&RemovedSpace> = std::iter::once(&record).chain(&carried).collect();
        let warnings = records.iter().copied().filter_map(no_session).collect();
        let kept_sessions = manager.kept_sessions_of(&records).await;
        let repos = manager.repositories_of(&records).await?;
        let manifest = manager.manifest_of(&repos, record, carried).await?;
        Ok(Self {
            repos,
            manifest,
            removal: None,
            settings,
            kept_sessions,
            warnings,
        })
    }
}

impl Manager {
    /// Write a space for another Shoal to the new file `file`, removing it
    /// here first with the spaces below it that `carrying` carries, of
    /// those `unsaved` confirms. A selector that names no space exports the
    /// removal record of the space with that ID, which stays, with those
    /// its removal took. The daemon writes the file, so closing the
    /// caller's terminal with the space cannot cut the export short; it
    /// checks that it can before ending anything, since its file access can
    /// differ from the caller's. Writing it that fails names the restore
    /// that opens the space here again.
    pub async fn export_space(
        &self,
        selector: &SpaceSelector,
        unsaved: UnsavedWork,
        inspection: InspectionPolicy,
        file: std::path::PathBuf,
        carrying: &Carrying,
    ) -> Result<ExportedSpace> {
        crate::daemon::access::ensure_writable(crate::transfer::directory(&file)).await?;
        let prepared = match self.space(selector).await {
            Ok(space) => (self.end_for_export(space, unsaved, inspection, carrying)).await?,
            Err(_) => self.prepare_kept_record(selector, carrying).await?,
        };
        let id = prepared.manifest.space.record.space_id.clone();
        (self.write_prepared(prepared, file).await)
            .with_context(|| format!("shoal restore {id} opens it here again"))
    }

    /// End `space` for its export through the removal path, with the
    /// spaces below it that `carrying` carries, stopping their agents, so
    /// their sessions' files stop changing and only writing them is left,
    /// and prepare the package of the records their removal keeps.
    async fn end_for_export(
        &self,
        space: Space,
        unsaved: UnsavedWork,
        inspection: InspectionPolicy,
        carrying: &Carrying,
    ) -> Result<Prepared> {
        let carried = self.carried_by(&space.id, carrying).await?;
        carried::ensure_packable(&space, &carried)?;
        // Its own config may describe its agents, and goes with it.
        let settings = self.space_settings(&space).await;
        let removal = (self.remove_packaged(&space, &carried, unsaved, inspection)).await?;
        let removed = self.removal_record(&space.id).await?;
        let carried = self.removal_records(&carried).await?;
        let mut prepared = self.prepare_removed(removed, carried, settings).await?;
        prepared.removal = Some(removal);
        Ok(prepared)
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
        let settings = self.space_settings_in(removed.repository_id.as_ref()).await;
        self.prepare_removed(removed, carried, settings).await
    }

    /// Prepare the package of the space `removed` records with the spaces
    /// `carried` records, as their removal kept them, whose agents
    /// `settings` describes.
    async fn prepare_removed(
        &self,
        removed: RemovedSpace,
        carried: Vec<RemovedSpace>,
        settings: Result<Effective>,
    ) -> Result<Prepared> {
        for record in std::iter::once(&removed).chain(&carried) {
            worktree::ensure_files_kept(record)?;
        }
        Prepared::of(self, removed, carried, settings).await
    }

    /// Remove `space` for its package through the removal path, deleting
    /// the `unsaved` work the caller confirmed and taking only the spaces
    /// `carried` with it among the spaces below that it confirms: a space
    /// that joins it meanwhile retains it.
    async fn remove_packaged(
        &self,
        space: &Space,
        carried: &[Space],
        unsaved: UnsavedWork,
        inspection: InspectionPolicy,
    ) -> Result<RemovalResult> {
        let confirmed = |id: &&SpaceId| unsaved.children().is_none_or(|ids| ids.contains(id));
        let carried = carried.iter().map(|space| &space.id);
        let taking: Vec<SpaceId> = carried.filter(confirmed).cloned().collect();
        let selector = SpaceSelector::id(&space.id);
        (self.remove_selected_taking(&selector, unsaved, inspection, &taking)).await
    }

    /// The repositories the spaces `records` belong to, each once.
    async fn repositories_of(&self, records: &[&RemovedSpace]) -> Result<Vec<Repository>> {
        let mut repos: Vec<Repository> = Vec::new();
        for id in records
            .iter()
            .filter_map(|record| record.repository_id.as_ref())
        {
            if !repos.iter().any(|repo| &repo.id == id) {
                repos.push(self.repository(id).await?);
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
            removal,
            settings,
            kept_sessions,
            mut warnings,
        } = prepared;
        let (manifest, left_out) = self
            .write_package(&repos, manifest, settings, kept_sessions, file)
            .await?;
        warnings.extend(left_out);
        Ok(ExportedSpace {
            manifest,
            removal,
            warnings,
        })
    }

    /// Write `manifest`'s package, spaces of `repos` or of none, whose
    /// agents `settings` describes, with their session files to the new
    /// file `file`, those of a session in `kept` from there, and the
    /// warnings for what it left out.
    async fn write_package(
        &self,
        repos: &[Repository],
        manifest: Manifest,
        settings: Result<Effective>,
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
    /// `repos` or of none, reported, as `settings` describes the agents,
    /// wherever they ran, with a warning for each session left out, which
    /// then starts over on the importing machine.
    async fn session_sources(
        &self,
        repos: &[Repository],
        settings: Result<Effective>,
        manifest: &Manifest,
    ) -> (Vec<SessionSource>, Vec<String>) {
        let settings = match settings {
            Ok(settings) => settings,
            Err(error) => {
                return (
                    Vec::new(),
                    vec![format!(
                        "cannot export the agents' session files: {error:#}"
                    )],
                );
            }
        };
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
        let environments = Environments::new(&self.paths.home, &environments);
        let sessions = manifest.every_session();
        let found = tokio::task::spawn_blocking(move || {
            crate::transfer::session_sources(&settings, &environments, &sessions)
        })
        .await;
        match found {
            Ok((sessions, more)) => {
                warnings.extend(more);
                (sessions, warnings)
            }
            Err(error) => {
                warnings.push(format!("cannot export the agents' session files: {error}"));
                (Vec::new(), warnings)
            }
        }
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
