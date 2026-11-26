//! Packages of spaces for `shoal move`: of a space that moves, a removed
//! one, or a copy that keeps running, each with the spaces below it that
//! the package carries. See `docs/design/moves.md`.
use crate::Spaces;
use crate::daemon::Daemon;
use crate::transfer::ManifestExt;
use anyhow::{Context, Result};
use shoal_ids::checkpoint_numbers::CheckpointNumber;
use shoal_ids::space_ids::SpaceId;
use std::collections::BTreeMap;

use crate::{
    config::Effective,
    model::{RemovedSpace, Repository, Space},
    removal::{InspectionPolicy, RemovalResult, UnsavedWork},
    transfer::{Carrying, ExportedSpace, Manifest, MoveDestination, SessionSource},
};
use shoal_protocol::space_names::SpaceSelector;
use shoal_protocol::transcripts::Environments;

mod carried;
mod copy;
pub mod kept;
mod moving;
pub use moving::{HELD_FOR_READER, MovePacking};
pub mod records;
mod retiring;
mod staying;
mod worktree;

/// A space prepared for its package, with the spaces it carries: their
/// repositories, the package without its session files, the settings that
/// describe their agents, the session files an import kept for them, and
/// what the package leaves out.
struct Prepared {
    repos: Vec<Repository>,
    manifest: Manifest,
    settings: Vec<Result<Effective>>,
    kept_sessions: Vec<SessionSource>,
    warnings: Vec<String>,
}

impl Prepared {
    /// `settings` describes the agents of `record` and of each of `carried`,
    /// one for each in that order.
    async fn of(
        daemon: &dyn Daemon,
        record: RemovedSpace,
        carried: Vec<RemovedSpace>,
        settings: Vec<Result<Effective>>,
    ) -> Result<Self> {
        let records: Vec<&RemovedSpace> = std::iter::once(&record).chain(&carried).collect();
        let warnings = records.iter().copied().filter_map(no_session).collect();
        let kept_sessions = daemon.spaces().kept_sessions_of(daemon, &records).await;
        let repos = daemon.spaces().repositories_of(daemon, &records).await?;
        let manifest = daemon.spaces().manifest_of(&repos, record, carried).await?;
        Ok(Self {
            repos,
            manifest,
            settings,
            kept_sessions,
            warnings,
        })
    }
}

impl Spaces {
    /// The removal record stays here. Also writes the records of the spaces
    /// its removal took that `carrying` carries. `file` must be new, and the
    /// daemon must be able to write there.
    pub async fn export_removed_space(
        &self,
        daemon: &dyn Daemon,
        selector: &SpaceSelector,
        file: std::path::PathBuf,
        carrying: &Carrying,
    ) -> Result<ExportedSpace> {
        shoal_daemon_base::writable::ensure_writable(crate::transfer::directory(&file)).await?;
        let prepared = self.prepare_kept_record(daemon, selector, carrying).await?;
        self.write_prepared(daemon, prepared, file).await
    }

    /// Writes to the package its move to a joined machine delivers from
    /// here, replacing one an earlier move of it left.
    pub async fn export_removed_moving(
        &self,
        daemon: &dyn Daemon,
        selector: &SpaceSelector,
        carrying: &Carrying,
    ) -> Result<ExportedSpace> {
        let prepared = self.prepare_kept_record(daemon, selector, carrying).await?;
        let file = self.new_staged_package(&prepared.manifest.space.record.space_id)?;
        self.write_prepared(daemon, prepared, file).await
    }

    /// Among the packages delivered from here. Holds none yet.
    fn new_staged_package(&self, id: &SpaceId) -> Result<std::path::PathBuf> {
        let staging = self.paths.move_packages();
        std::fs::create_dir_all(&staging)
            .with_context(|| format!("create {}", staging.display()))?;
        let file = self.staged_package(id);
        // The caller holds the space's move lock, so a move that stopped
        // left this one.
        if let Err(error) = std::fs::remove_file(&file)
            && error.kind() != std::io::ErrorKind::NotFound
        {
            return Err(error).with_context(|| format!("remove {}", file.display()));
        }
        Ok(file)
    }

    /// The removal record stays here.
    async fn prepare_kept_record(
        &self,
        daemon: &dyn Daemon,
        selector: &SpaceSelector,
        carrying: &Carrying,
    ) -> Result<Prepared> {
        let (removed, carried) = self.record_with_taken(selector, carrying).await?;
        let mut settings = Vec::new();
        for record in std::iter::once(&removed).chain(&carried) {
            settings.push(
                daemon
                    .space_settings_in(record.repository_id.as_ref())
                    .await,
            );
        }
        self.prepare_removed(daemon, removed, carried, settings)
            .await
    }

    /// The removal records of the spaces its removal took that `carrying`
    /// carries, which its record's package carries.
    pub async fn taken_records(
        &self,
        selector: &SpaceSelector,
        carrying: &Carrying,
    ) -> Result<Vec<RemovedSpace>> {
        Ok(self.record_with_taken(selector, carrying).await?.1)
    }

    async fn record_with_taken(
        &self,
        selector: &SpaceSelector,
        carrying: &Carrying,
    ) -> Result<(RemovedSpace, Vec<RemovedSpace>)> {
        let records = self.removed_spaces().await?;
        let removed = (records.iter())
            .find(|removed| selector.given_id() == Some(&removed.space_id))
            .with_context(|| format!("no space or restorable space {selector}"))?
            .clone();
        let carried = carried::taken_by_removal(&records, &removed.space_id, carrying);
        Ok((removed, carried))
    }

    /// As their removal kept them. `settings` describes the agents of each.
    async fn prepare_removed(
        &self,
        daemon: &dyn Daemon,
        removed: RemovedSpace,
        carried: Vec<RemovedSpace>,
        settings: Vec<Result<Effective>>,
    ) -> Result<Prepared> {
        for record in std::iter::once(&removed).chain(&carried) {
            worktree::ensure_files_kept(record)?;
        }
        Prepared::of(daemon, removed, carried, settings).await
    }

    /// Leaves them as [`Self::leave_behind`] does. A move to a joined machine
    /// instead keeps them below the spaces they are below, detached until
    /// the removal here ends, when [`Self::attach_behind`] links them again.
    /// Carrying every space below leaves none, and one that joins meanwhile
    /// retains the space.
    async fn leave_uncarried(
        &self,
        daemon: &dyn Daemon,
        space: &Space,
        carried: &[Space],
        (carrying, destination): (&Carrying, &MoveDestination),
    ) -> Result<()> {
        match (carrying, destination) {
            (Carrying::Subtree, _) => Ok(()),
            (Carrying::Selected(_), MoveDestination::Machine(_)) => {
                self.detach_behind(space, carried).await
            }
            (Carrying::Selected(_), _) => self.leave_behind(daemon, space, carried).await,
        }
    }

    /// Deletes the `unsaved` work the caller confirmed and takes only the
    /// spaces `carried` among those below that it confirms: a space that
    /// joins meanwhile retains it. For a move another machine imported,
    /// `moved` holds each workspace's checkpoint in its package, from which
    /// none may differ once its removal hooks ran.
    async fn remove_packaged(
        &self,
        daemon: &dyn Daemon,
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
        (self.remove_selected_taking(daemon, &selector, unsaved, inspection, &taking, moved)).await
    }

    /// Each once.
    async fn repositories_of(
        &self,
        daemon: &dyn Daemon,
        records: &[&RemovedSpace],
    ) -> Result<Vec<Repository>> {
        let mut repos: Vec<Repository> = Vec::new();
        for id in records
            .iter()
            .filter_map(|record| record.repository_id.as_ref())
        {
            if !repos.iter().any(|repo| &repo.id == id) {
                repos.push(
                    daemon
                        .repository(crate::repository::Selector::from(id))
                        .await?,
                );
            }
        }
        Ok(repos)
    }

    async fn kept_sessions_of(
        &self,
        daemon: &dyn Daemon,
        records: &[&RemovedSpace],
    ) -> Vec<SessionSource> {
        let mut kept = Vec::new();
        for record in records {
            kept.extend(self.kept_session_sources(daemon, record).await);
        }
        kept
    }

    /// `file` must be new.
    async fn write_prepared(
        &self,
        daemon: &dyn Daemon,
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
            .write_package(daemon, &repos, manifest, settings, kept_sessions, file)
            .await?;
        warnings.extend(left_out);
        Ok(ExportedSpace { manifest, warnings })
    }

    /// `settings` describes the agents of each space the package holds.
    /// Writes their session files, those of a session in `kept` from there,
    /// and the warnings for what it left out. `file` must be new.
    async fn write_package(
        &self,
        daemon: &dyn Daemon,
        repos: &[Repository],
        manifest: Manifest,
        settings: Vec<Result<Effective>>,
        kept: Vec<SessionSource>,
        file: std::path::PathBuf,
    ) -> Result<(Manifest, Vec<String>)> {
        let (mut sessions, mut warnings) = self
            .session_sources(daemon, repos, settings, &manifest)
            .await;
        sessions.retain(|found| !kept.iter().any(|kept| kept.session == found.session));
        sessions.extend(kept);
        let (manifest, left_out) = crate::transfer::export(repos, manifest, sessions, file).await?;
        warnings.extend(left_out);
        Ok((manifest, warnings))
    }

    /// Wherever the agents ran. A session left out gets a warning, and
    /// starts over on the importing machine.
    async fn session_sources(
        &self,
        daemon: &dyn Daemon,
        repos: &[Repository],
        settings: Vec<Result<Effective>>,
        manifest: &Manifest,
    ) -> (Vec<SessionSource>, Vec<String>) {
        let (environments, mut warnings) = self.package_environments(daemon, repos).await;
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

    /// Where their session files are. Warns when one cannot be read.
    async fn package_environments(
        &self,
        daemon: &dyn Daemon,
        repos: &[Repository],
    ) -> (Environments, Vec<String>) {
        let mut warnings = Vec::new();
        // Only a repository records the environments its agents ran in.
        let mut environments = Vec::new();
        for repo in repos {
            match daemon.agent_environments(&repo.id).await {
                Ok(recorded) => environments.extend(recorded),
                Err(error) => warnings.push(format!(
                    "agents' session files are looked up in the default places only: {error:#}"
                )),
            }
        }
        (Environments::new(&self.paths.home, &environments), warnings)
    }
}

/// For a space whose agent saved no session to continue, which starts as a
/// new one where it is imported.
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
