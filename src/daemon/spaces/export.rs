//! Packages of spaces for `shoal move`: the daemon removes a space here,
//! stopping a chat's agent first, or copies one that keeps running, and
//! writes what continues it on another machine.
use crate::client::space_ids::SpaceId;
use crate::transfer::ManifestExt;
use anyhow::{Context, Result, ensure};

use crate::client::space_names::SpaceSelector;
use crate::{
    client::transcripts::Environments,
    config::Effective,
    daemon::workspace::{Manager, kept_session_sources},
    model::{Chat, PresentSpace, Repository, SpaceKind, Workspace},
    removal::{InspectionPolicy, RemovalResult, UnsavedWork},
    transfer::{ExportedSpace, Manifest, SessionSource, Source},
};

mod copy;

/// A space prepared for its package, ended here or copied: its repository,
/// the package without its session files, the removal that ended a
/// workspace, the settings describing its agents when they are not the
/// repository's, the session files an import kept for it, and how the user
/// continues the space here when writing the package fails, unless a
/// removal record or the space itself keeps it.
struct Prepared {
    repo: Repository,
    manifest: Manifest,
    removal: Option<RemovalResult>,
    settings: Option<Result<Effective>>,
    kept_sessions: Vec<SessionSource>,
    continues: Option<String>,
}

impl Manager {
    /// Write a space for another Shoal to the new file `file`, removing it
    /// here first, a running chat once its agent stopped. A selector that names neither exports the latest removal
    /// record with that name or ID. The daemon writes the file, so closing
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
            Ok(PresentSpace::Chat(chat)) => self.end_chat_for_export(chat).await?,
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
        checkpoint: Option<u64>,
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
        let settings = match prepared.settings {
            Some(settings) => settings,
            None => self.repository_settings(&prepared.repo.id).await,
        };
        let written = self
            .write_package(
                &prepared.repo,
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

    /// Write `manifest`'s package, a space of `repo` whose agents `settings`
    /// describes, with their session files to the new file `file`, those
    /// of a session in `kept` from there, and the warnings for what it left
    /// out.
    async fn write_package(
        &self,
        repo: &Repository,
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
    /// latest removal record with that name or ID when none exists.
    async fn remove_for_export(
        &self,
        selector: &SpaceSelector,
        unsaved: UnsavedWork,
        inspection: InspectionPolicy,
    ) -> Result<Prepared> {
        let (removal, settings) = match self.workspace(selector).await {
            Ok(workspace) => {
                // The package is built from the removal record, which a
                // disposable workspace's removal does not keep.
                anyhow::ensure!(
                    !workspace.disposable,
                    "workspace {} is disposable, so its removal keeps nothing to export",
                    workspace.name
                );
                let taking = self.exported_with(&workspace, &unsaved).await?;
                // Its own config may describe its agents, and goes with it.
                let settings = self.workspace_settings(&workspace).await;
                let removal = self
                    .remove_workspace_taking(
                        &SpaceSelector::id(&workspace.id),
                        unsaved,
                        inspection,
                        &taking,
                    )
                    .await?;
                (Some((workspace.id.clone(), removal)), Some(settings))
            }
            Err(_) => (None, None),
        };
        let removed = self
            .removed_workspaces()
            .await?
            .into_iter()
            .find(|removed| match &removal {
                Some((id, _)) => &removed.workspace_id == id,
                None => selector.names(&removed.workspace_id, &removed.name),
            })
            .with_context(|| format!("no space or restorable workspace {selector}"))?;
        ensure!(
            !removed.checkpoint.files_omitted,
            "the removal could not keep the uncommitted files; shoal work restore {} opens the workspace here again",
            removed.name
        );
        let repo = self.repository(&removed.repository_id).await?;
        Ok(Prepared {
            kept_sessions: kept_session_sources(&repo, &removed),
            manifest: Manifest::of(&repo, Source::Workspace(removed)).await?,
            repo,
            removal: removal.map(|(_, removal)| removal),
            settings,
            continues: None,
        })
    }

    /// The IDs of the spaces the export of `workspace` removes first, of
    /// those `unsaved` confirms, refusing while chats run in its worktree or
    /// in one below it, which its removal would take without exporting
    /// them. The removal takes only these, so a chat that starts meanwhile
    /// retains the workspace.
    async fn exported_with(
        &self,
        workspace: &Workspace,
        unsaved: &UnsavedWork,
    ) -> Result<Vec<SpaceId>> {
        let (chats, taken): (Vec<_>, Vec<_>) = (self.spaces_taken_with(&workspace.id).await?)
            .into_iter()
            .partition(|space| space.kind == SpaceKind::Chat);
        let chats: Vec<String> = chats.into_iter().map(|chat| chat.name).collect();
        ensure!(
            chats.is_empty(),
            "chats run in workspace {} or below it ({}), which shoal move does not move; remove them first",
            workspace.name,
            chats.join(", ")
        );
        let confirmed = |id: &SpaceId| unsaved.children().is_none_or(|ids| ids.contains(id));
        Ok(taken
            .into_iter()
            .map(|space| space.id)
            .filter(confirmed)
            .collect())
    }

    /// Remove `chat` for its export, which takes no space below it, once its
    /// agent has reported the session the package continues and its package
    /// is made, stopping a running one, so the session's files stop changing
    /// and only writing them is left.
    async fn end_chat_for_export(&self, chat: Chat) -> Result<Prepared> {
        if let Some(workspace) = &chat.workspace {
            anyhow::bail!(
                "chat {} runs in workspace {}, and shoal move does not move a chat in a workspace",
                chat.name,
                workspace.name
            );
        }
        let Some(repo) = self.chat_repository(&chat).await? else {
            anyhow::bail!(
                "chat {} has no repository, and shoal move moves only a chat in one",
                chat.name
            );
        };
        let Some(continues) = crate::transfer::continue_chat(&chat, crate::repository::name(&repo))
        else {
            anyhow::bail!(
                "the agent of chat {} reported no session to continue elsewhere, so the chat keeps running",
                chat.name
            );
        };
        ensure!(
            self.children(&chat.id).await?.is_empty(),
            "chat {} has spaces below it; move or remove them first",
            chat.name
        );
        let id = chat.id.clone();
        let manifest = Manifest::of(&repo, Source::Chat(chat)).await?;
        self.remove_chat(&SpaceSelector::id(&id), Some(Vec::new()))
            .await?;
        Ok(Prepared {
            repo,
            manifest,
            removal: None,
            settings: None,
            kept_sessions: Vec::new(),
            continues: Some(continues),
        })
    }

    /// The files of the sessions the agents of the package `manifest`
    /// reported, as `settings` describes the agents, wherever they ran, with
    /// a warning for each session left out, which then starts over on the
    /// importing machine.
    async fn session_sources(
        &self,
        repo: &Repository,
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
        let environments = self
            .agent_environments(&repo.id)
            .await
            .unwrap_or_else(|error| {
                warnings.push(format!(
                    "agents' session files are looked up in the default places only: {error:#}"
                ));
                Vec::new()
            });
        let environments = Environments::new(&self.paths.home, &environments);
        let sessions = manifest.sessions.clone().unwrap_or_default();
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

#[cfg(test)]
mod tests;
