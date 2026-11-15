//! Packages of spaces for `shoal move`: the daemon removes a space here,
//! stopping a chat's agent first, copies one that keeps running, or stops
//! one's agents for a move that ends it once imported, and writes what
//! continues it on another machine.
use crate::execution::recovery::LastSessionExt;
use crate::transfer::ManifestExt;
use anyhow::{Context, Result, ensure};
use shoal_ids::space_ids::SpaceId;

use crate::client::space_names::SpaceSelector;
use crate::{
    client::transcripts::Environments,
    config::Effective,
    daemon::{
        events::EventCause,
        workspace::{Manager, kept_session_sources},
    },
    execution::recovery::{LastSession, StoppedAgent},
    model::{
        Chat, Checkpoint, KeptWorkspace, PresentSpace, RemovedKind, RemovedSpace, Repository,
        Workspace,
    },
    paths::Paths,
    removal::{InspectionPolicy, RemovalResult, UnsavedWork},
    time::UnixTime,
    transfer::{ExportedSpace, Manifest, SessionSource, Source},
};
use shoal_ids::checkpoint_numbers::CheckpointNumber;

mod carried;
mod copy;
mod moving;

/// A space prepared for its package, ended here or copied: its repository,
/// none for a chat without one, the package without its session files, the
/// removal that ended a workspace, the settings describing its agents when
/// they are not the repository's, the session files an import kept for it,
/// and how the user continues the space here when writing the package
/// fails, unless a removal record or the space itself keeps it.
struct Prepared {
    repo: Option<Repository>,
    manifest: Manifest,
    removal: Option<RemovalResult>,
    settings: Option<Result<Effective>>,
    kept_sessions: Vec<SessionSource>,
    continues: Option<String>,
}

impl Manager {
    /// Write a space for another Shoal to the new file `file`, removing it
    /// here first with the spaces below it that `unsaved` confirms, a
    /// running chat once its agent stopped. A selector that names neither
    /// exports the removal record of the workspace or chat with that ID,
    /// which stays. The daemon writes the file, so closing
    /// the caller's terminal with the space cannot cut the export short; it
    /// checks that it can before ending anything, since its file access can
    /// differ from the caller's.
    pub async fn export_space(
        &self,
        selector: &SpaceSelector,
        unsaved: UnsavedWork,
        inspection: InspectionPolicy,
        file: std::path::PathBuf,
    ) -> Result<ExportedSpace> {
        crate::daemon::access::ensure_writable(crate::transfer::directory(&file)).await?;
        let ended = match self.space(selector).await {
            Ok(PresentSpace::Chat(chat)) => self.end_chat_for_export(chat, &unsaved).await?,
            _ => {
                self.remove_for_export(selector, unsaved, inspection)
                    .await?
            }
        };
        self.write_prepared(ended, file).await
    }

    /// Write a copy of the space `selector` names for another Shoal to the
    /// new file `file`, leaving the space running here: a workspace at its
    /// checkpoint `checkpoint`, or at a new one, or a chat with its session.
    pub async fn copy_space(
        &self,
        selector: &SpaceSelector,
        checkpoint: Option<CheckpointNumber>,
        file: std::path::PathBuf,
    ) -> Result<ExportedSpace> {
        crate::daemon::access::ensure_writable(crate::transfer::directory(&file)).await?;
        let copied = match self.running_space(selector).await? {
            PresentSpace::Workspace(workspace) => {
                self.copy_workspace(workspace, checkpoint).await?
            }
            PresentSpace::Chat(chat) => {
                ensure!(
                    checkpoint.is_none(),
                    "chat {} has no checkpoints; export a copy of it without --checkpoint",
                    chat.name
                );
                self.copy_chat(chat).await?
            }
        };
        self.write_prepared(copied, file).await
    }

    /// Write the package of `prepared` to the new file `file`.
    async fn write_prepared(
        &self,
        prepared: Prepared,
        file: std::path::PathBuf,
    ) -> Result<ExportedSpace> {
        let repo = prepared.repo.as_ref();
        let settings = match prepared.settings {
            Some(settings) => settings,
            None => self.space_settings_in(repo.map(|repo| &repo.id)).await,
        };
        let written = self
            .write_package(
                repo,
                prepared.manifest,
                settings,
                prepared.kept_sessions,
                file,
            )
            .await;
        let (manifest, warnings) = match (written, prepared.continues) {
            (Ok(written), _) => written,
            (Err(error), Some(continues)) => return Err(error.context(continues)),
            (Err(error), None) => return Err(error),
        };
        Ok(ExportedSpace {
            manifest,
            removal: prepared.removal,
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

    /// Remove the workspace `selector` names for its export, or take the
    /// removal record of the workspace or chat with that ID when none
    /// exists, which stays.
    async fn remove_for_export(
        &self,
        selector: &SpaceSelector,
        unsaved: UnsavedWork,
        inspection: InspectionPolicy,
    ) -> Result<Prepared> {
        let ended = match self.workspace(selector).await {
            Ok(workspace) => {
                Some((self.end_workspace_for_export(&workspace, unsaved, inspection)).await?)
            }
            Err(_) => None,
        };
        let removed = self
            .removed_spaces()
            .await?
            .into_iter()
            .find(|removed| match &ended {
                Some(ended) => removed.space_id == ended.id,
                None => selector.given_id() == Some(&removed.space_id),
            })
            .with_context(|| format!("no space or restorable space {selector}"))?;
        match removed.kind {
            RemovedKind::Workspace(_) => self.prepare_removed_workspace(removed, ended).await,
            RemovedKind::Chat { .. } => self.prepare_removed_chat(removed).await,
        }
    }

    /// Remove `workspace` for its export with the spaces below it that
    /// `unsaved` confirms, naming the chats that go with it.
    async fn end_workspace_for_export(
        &self,
        workspace: &Workspace,
        unsaved: UnsavedWork,
        inspection: InspectionPolicy,
    ) -> Result<EndedWorkspace> {
        ensure_exportable(workspace)?;
        // Its own config may describe its agents, and goes with it.
        let settings = self.workspace_settings(workspace).await;
        let carried = self.chats_carried_by(&workspace.id).await?;
        let removal = (self.remove_exported(workspace, unsaved, inspection)).await?;
        Ok(EndedWorkspace {
            id: workspace.id.clone(),
            removal,
            settings,
            carried,
        })
    }

    /// Prepare the package of the workspace `removed` records, which the
    /// export `ended` when it did, refusing one whose removal could not keep
    /// its uncommitted files.
    async fn prepare_removed_workspace(
        &self,
        removed: RemovedSpace,
        ended: Option<EndedWorkspace>,
    ) -> Result<Prepared> {
        ensure!(
            !removed.kept_workspace()?.checkpoint.files_omitted,
            "the removal could not keep the uncommitted files; shoal restore {} opens the workspace here again",
            removed.space_id
        );
        let (removal, settings, carried) = match ended {
            Some(ended) => (Some(ended.removal), Some(ended.settings), ended.carried),
            None => (None, None, Vec::new()),
        };
        let repo = self.repository(removed.repository()?).await?;
        let kept_sessions = kept_session_sources(&repo, &removed);
        let mut manifest = Manifest::of(Some(&repo), Source::Workspace(removed)).await?;
        manifest.chats = self.carried_records(&carried).await?;
        Ok(Prepared {
            kept_sessions,
            manifest,
            repo: Some(repo),
            removal,
            settings,
            continues: None,
        })
    }

    /// Prepare the package of the chat `removed` records, whose record
    /// stays.
    async fn prepare_removed_chat(&self, removed: RemovedSpace) -> Result<Prepared> {
        let repo = self.movable_removed_chat(&removed).await?;
        Ok(Prepared {
            manifest: Manifest::of(repo.as_ref(), Source::RemovedChat(removed)).await?,
            repo,
            removal: None,
            settings: None,
            kept_sessions: Vec::new(),
            continues: None,
        })
    }

    /// The repository of the chat `removed` records, which moves to another
    /// machine, none for a chat without one, refusing what
    /// [`Self::movable_chat`] refuses of a live chat that its removal left:
    /// one whose agent reported no session to continue.
    async fn movable_removed_chat(&self, removed: &RemovedSpace) -> Result<Option<Repository>> {
        let kept = removed
            .chat()
            .context("the removal record is not a chat's")?;
        ensure!(
            kept.session.is_some(),
            "the agent of chat {} reported no session to continue elsewhere; shoal restore {} opens it here again",
            removed.name,
            removed.space_id
        );
        match &removed.repository_id {
            Some(repository) => self.repository(repository).await.map(Some),
            None => Ok(None),
        }
    }

    /// Remove `workspace` for its export through the removal path, taking
    /// the spaces below it that `unsaved` confirms.
    async fn remove_exported(
        &self,
        workspace: &Workspace,
        unsaved: UnsavedWork,
        inspection: InspectionPolicy,
    ) -> Result<RemovalResult> {
        let taking = self.exported_with(workspace, &unsaved).await?;
        let selector = SpaceSelector::id(&workspace.id);
        (self.remove_workspace_taking(&selector, unsaved, inspection, &taking)).await
    }

    /// The IDs of the spaces the export of `workspace` removes first, of
    /// those `unsaved` confirms. The removal takes only these, so a space
    /// that joins it meanwhile retains the workspace.
    async fn exported_with(
        &self,
        workspace: &Workspace,
        unsaved: &UnsavedWork,
    ) -> Result<Vec<SpaceId>> {
        let confirmed = |id: &SpaceId| unsaved.children().is_none_or(|ids| ids.contains(id));
        Ok((self.spaces_taken_with(&workspace.id).await?)
            .into_iter()
            .map(|space| space.id)
            .filter(confirmed)
            .collect())
    }

    /// Remove `chat` for its export, with the chats that go with it, of
    /// those `unsaved` confirms, once its agent has reported the session the
    /// package continues and its package is made, stopping the running
    /// agents, so their sessions' files stop changing and only writing them
    /// is left. A chat that joins it meanwhile keeps it here.
    async fn end_chat_for_export(&self, chat: Chat, unsaved: &UnsavedWork) -> Result<Prepared> {
        let (repo, continues) = self.movable_chat(&chat).await?;
        let carried = self.chats_carried_by(&chat.id).await?;
        let taking = self.chat_taking(&chat.id, unsaved).await?;
        let id = chat.id.clone();
        let mut manifest = Manifest::of(repo.as_ref(), Source::Chat(chat)).await?;
        (self.remove_chat(&SpaceSelector::id(&id), Some(taking))).await?;
        manifest.chats = self.carried_records(&carried).await?;
        Ok(Prepared {
            repo,
            manifest,
            removal: None,
            settings: None,
            kept_sessions: Vec::new(),
            continues: Some(continues),
        })
    }

    /// The repository of `chat`, which moves to another machine, none for a
    /// chat without one, and how the user continues it here, refusing a chat
    /// whose agent reported no session to continue and one with a space
    /// below it that does not go with it.
    async fn movable_chat(&self, chat: &Chat) -> Result<(Option<Repository>, String)> {
        let repo = self.chat_repository(chat).await?;
        let Some(continues) =
            crate::transfer::continue_chat(chat, repo.as_ref().map(crate::repository::name))
        else {
            anyhow::bail!(
                "the agent of chat {} reported no session to continue elsewhere, so the chat keeps running",
                chat.name
            );
        };
        self.ensure_only_carried_below(&chat.id, &chat.name).await?;
        Ok((repo, continues))
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

/// A workspace the export removed: its ID, its removal, the settings
/// describing its agents and the chats that went with it.
struct EndedWorkspace {
    id: SpaceId,
    removal: RemovalResult,
    settings: Result<Effective>,
    carried: Vec<SpaceId>,
}

/// Refuse to export `workspace` when disposable: the package is built from
/// its removal record, which a disposable workspace's removal does not keep.
fn ensure_exportable(workspace: &Workspace) -> Result<()> {
    ensure!(
        !workspace.disposable,
        "workspace {} is disposable, so its removal keeps nothing to export",
        workspace.name
    );
    Ok(())
}

/// The record a package of `workspace`, which keeps running, holds: its
/// `checkpoint` and `agents` with the last sessions it keeps, as its
/// removal would record them.
fn packaged_record(
    paths: &Paths,
    workspace: &Workspace,
    checkpoint: Checkpoint,
    agents: Vec<StoppedAgent>,
) -> RemovedSpace {
    RemovedSpace {
        space_id: workspace.id.clone(),
        repository_id: Some(workspace.repository_id.clone()),
        name: workspace.name.clone(),
        expires_at: checkpoint.expires_at,
        parent: workspace.parent.clone(),
        links: workspace.links.clone(),
        agents,
        // An unreadable last session is not worth refusing the export for.
        last_sessions: LastSession::all(paths, &workspace.id).unwrap_or_default(),
        session_files: Vec::new(),
        cause: EventCause::Manual,
        removed_at: UnixTime::now(),
        kind: RemovedKind::Workspace(Box::new(KeptWorkspace {
            checkpoint,
            base_ref: workspace.base_ref.clone(),
            base_commit: workspace.base_commit.clone(),
            at: workspace.at.clone(),
            setup_skipped: workspace.setup_skipped,
        })),
    }
}

#[cfg(test)]
mod tests;
