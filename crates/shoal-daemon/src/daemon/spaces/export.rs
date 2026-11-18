//! Packages of spaces for `shoal move`: the daemon removes a space here,
//! copies one that keeps running, or stops one's agents for a move that
//! ends it once imported, and writes what continues it on another machine.
//! Each runs one path for a space of either kind; what a workspace's
//! worktree adds is its worktree step.
use crate::transfer::ManifestExt;
use anyhow::{Context, Result};
use shoal_ids::repository_ids::RepositoryId;
use shoal_ids::space_ids::SpaceId;

use crate::{
    config::Effective,
    daemon::workspace::Manager,
    model::{RemovedSpace, Repository, Space},
    removal::{InspectionPolicy, RemovalResult, UnsavedWork},
    transfer::{ExportedSpace, Manifest, SessionSource},
};
use shoal_protocol::space_names::SpaceSelector;
use shoal_protocol::transcripts::Environments;

mod carried;
mod copy;
mod moving;
mod worktree;

/// A space prepared for its package, ended here or copied: its repository,
/// none for a chat without one, the package without its session files, the
/// removal that ended it, the settings describing its agents, the session
/// files an import kept for it, and what the package leaves out.
struct Prepared {
    repo: Option<Repository>,
    manifest: Manifest,
    removal: Option<RemovalResult>,
    settings: Result<Effective>,
    kept_sessions: Vec<SessionSource>,
    warnings: Vec<String>,
}

impl Prepared {
    /// The package of the space `record` records, of `repo` or of none,
    /// whose agents `settings` describes, with the session files an import
    /// kept for it.
    async fn of(
        manager: &Manager,
        repo: Option<Repository>,
        record: RemovedSpace,
        settings: Result<Effective>,
    ) -> Result<Self> {
        let warnings = no_session(&record).into_iter().collect();
        let kept_sessions = manager.kept_session_sources(&record).await;
        Ok(Self {
            manifest: Manifest::of(repo.as_ref(), record).await?,
            repo,
            removal: None,
            settings,
            kept_sessions,
            warnings,
        })
    }
}

/// How an export ended its space: the removal, the settings describing its
/// agents, read before it, and the chats that went with it.
struct Ended {
    removal: Option<RemovalResult>,
    settings: Result<Effective>,
    carried: Vec<SpaceId>,
}

impl Manager {
    /// Write a space for another Shoal to the new file `file`, removing it
    /// here first with the chats that go with it, of those `unsaved`
    /// confirms. A selector that names no space exports the removal record
    /// of the space with that ID, which stays. The daemon writes the file,
    /// so closing the caller's terminal with the space cannot cut the
    /// export short; it checks that it can before ending anything, since
    /// its file access can differ from the caller's. Writing it that fails
    /// names the restore that opens the space here again.
    pub async fn export_space(
        &self,
        selector: &SpaceSelector,
        unsaved: UnsavedWork,
        inspection: InspectionPolicy,
        file: std::path::PathBuf,
    ) -> Result<ExportedSpace> {
        crate::daemon::access::ensure_writable(crate::transfer::directory(&file)).await?;
        let prepared = match self.space(selector).await {
            Ok(space) => self.end_for_export(space, unsaved, inspection).await?,
            Err(_) => self.prepare_kept_record(selector).await?,
        };
        let id = prepared.manifest.space.record.space_id.clone();
        (self.write_prepared(prepared, file).await)
            .with_context(|| format!("shoal restore {id} opens it here again"))
    }

    /// End `space` for its export through the removal path, stopping its
    /// agents, so their sessions' files stop changing and only writing them
    /// is left, and prepare the package of the record its removal keeps. A
    /// space below it that does not go with it refuses the export.
    async fn end_for_export(
        &self,
        space: Space,
        unsaved: UnsavedWork,
        inspection: InspectionPolicy,
    ) -> Result<Prepared> {
        worktree::ensure_packable(&space)?;
        self.ensure_only_carried_below(&space.id, &space.name)
            .await?;
        // Its own config may describe its agents, and goes with it.
        let settings = self.space_settings(&space).await;
        let carried = self.chats_carried_by(&space.id).await?;
        let removal = (self.remove_packaged(&space, &carried, unsaved, inspection)).await?;
        let removed = self.removal_record(&space.id).await?;
        let ended = Ended {
            removal: Some(removal),
            settings,
            carried,
        };
        self.prepare_removed(removed, ended).await
    }

    /// Prepare the package of the removal record of the space `selector`
    /// names by ID, which stays.
    async fn prepare_kept_record(&self, selector: &SpaceSelector) -> Result<Prepared> {
        let removed = (self.removed_spaces().await?.into_iter())
            .find(|removed| selector.given_id() == Some(&removed.space_id))
            .with_context(|| format!("no space or restorable space {selector}"))?;
        let ended = Ended {
            removal: None,
            settings: self.space_settings_in(removed.repository_id.as_ref()).await,
            carried: Vec::new(),
        };
        self.prepare_removed(removed, ended).await
    }

    /// Prepare the package of the space `removed` records, as `ended` ended
    /// it here, with the chats that went with it.
    async fn prepare_removed(&self, removed: RemovedSpace, ended: Ended) -> Result<Prepared> {
        worktree::ensure_files_kept(&removed)?;
        let repo = self.repository_of(removed.repository_id.as_ref()).await?;
        let mut prepared = Prepared::of(self, repo, removed, ended.settings).await?;
        prepared.manifest.chats = self.carried_records(&ended.carried).await?;
        prepared.removal = ended.removal;
        Ok(prepared)
    }

    /// Remove `space` for its package through the removal path, deleting
    /// the `unsaved` work the caller confirmed and taking only the chats
    /// `carried` with it among the spaces below that it confirms: a space
    /// that joins it meanwhile retains it.
    async fn remove_packaged(
        &self,
        space: &Space,
        carried: &[SpaceId],
        unsaved: UnsavedWork,
        inspection: InspectionPolicy,
    ) -> Result<RemovalResult> {
        let confirmed = |id: &&SpaceId| unsaved.children().is_none_or(|ids| ids.contains(id));
        let taking: Vec<SpaceId> = carried.iter().filter(confirmed).cloned().collect();
        let selector = SpaceSelector::id(&space.id);
        (self.remove_selected_taking(&selector, unsaved, inspection, &taking)).await
    }

    /// The repository `id`, none for a space without one.
    async fn repository_of(&self, id: Option<&RepositoryId>) -> Result<Option<Repository>> {
        match id {
            Some(id) => self.repository(id).await.map(Some),
            None => Ok(None),
        }
    }

    /// Write the package of `prepared` to the new file `file`.
    async fn write_prepared(
        &self,
        prepared: Prepared,
        file: std::path::PathBuf,
    ) -> Result<ExportedSpace> {
        let Prepared {
            repo,
            manifest,
            removal,
            settings,
            kept_sessions,
            mut warnings,
        } = prepared;
        let (manifest, left_out) = self
            .write_package(repo.as_ref(), manifest, settings, kept_sessions, file)
            .await?;
        warnings.extend(left_out);
        Ok(ExportedSpace {
            manifest,
            removal,
            warnings,
        })
    }

    /// Write `manifest`'s package, a space of `repo` or of none, whose
    /// agents `settings` describes, with their session files to the new
    /// file `file`, those of a session in `kept` from there, and the
    /// warnings for what it left out.
    async fn write_package(
        &self,
        repo: Option<&Repository>,
        manifest: Manifest,
        settings: Result<Effective>,
        kept: Vec<SessionSource>,
        file: std::path::PathBuf,
    ) -> Result<(Manifest, Vec<String>)> {
        let (mut sessions, mut warnings) = self.session_sources(repo, settings, &manifest).await;
        sessions.retain(|found| !kept.iter().any(|kept| kept.session == found.session));
        sessions.extend(kept);
        let (manifest, left_out) = crate::transfer::export(repo, manifest, sessions, file).await?;
        warnings.extend(left_out);
        Ok((manifest, warnings))
    }

    /// The files of the sessions the agents of the package `manifest`, of
    /// `repo` or of none, reported, as `settings` describes the agents,
    /// wherever they ran, with a warning for each session left out, which
    /// then starts over on the importing machine.
    async fn session_sources(
        &self,
        repo: Option<&Repository>,
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
        let environments = match repo {
            Some(repo) => self.agent_environments(&repo.id).await,
            // Only a repository records the environments its agents ran in.
            None => Ok(Vec::new()),
        };
        let environments = environments.unwrap_or_else(|error| {
            warnings.push(format!(
                "agents' session files are looked up in the default places only: {error:#}"
            ));
            Vec::new()
        });
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
