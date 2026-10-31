//! Packages of spaces for `shoal move`: the daemon ends a space here, a
//! workspace by its removal and a chat by stopping its agent, and writes
//! what continues it on another machine.
use anyhow::{Context, Result, ensure};

use crate::{
    config::Effective,
    daemon::workspace::Manager,
    model::{Chat, PresentSpace, Repository},
    removal::{InspectionPolicy, RemovalResult, UnsavedWork},
    transfer::{ExportedSpace, Manifest, SessionSource, Source},
};

/// A space ended here for its package: its repository, the package without
/// its session files, the removal that ended a workspace, the settings
/// describing its agents when they are not the repository's, and how the
/// user continues the space here when writing the package fails, unless a
/// removal record keeps it.
struct Ended {
    repo: Repository,
    manifest: Manifest,
    removal: Option<RemovalResult>,
    settings: Option<Result<Effective>>,
    continues: Option<String>,
}

impl Manager {
    /// Write a space for another Shoal to the new file `file`, ending it
    /// here first: a workspace by its removal, a running chat by stopping
    /// its agent. A selector that names neither exports the latest removal
    /// record with that name or ID. The daemon writes the file, so closing
    /// the caller's terminal with the space cannot cut the export short; it
    /// checks that it can before ending anything, since its file access can
    /// differ from the caller's.
    pub async fn export_space(
        &self,
        selector: &str,
        unsaved: UnsavedWork,
        inspection: InspectionPolicy,
        file: std::path::PathBuf,
    ) -> Result<ExportedSpace> {
        crate::daemon::access::ensure_writable(crate::transfer::directory(&file)).await?;
        let ended = match self.present_space(selector).await {
            Ok(PresentSpace::Chat(chat)) => self.end_chat_for_export(chat).await?,
            _ => {
                self.remove_for_export(selector, unsaved, inspection)
                    .await?
            }
        };
        let settings = match ended.settings {
            Some(settings) => settings,
            None => self.repository_settings(&ended.repo.id).await,
        };
        let written = self
            .write_package(&ended.repo, ended.manifest, settings, file)
            .await;
        let (manifest, warnings) = match (written, ended.continues) {
            (Ok(written), _) => written,
            (Err(error), Some(continues)) => return Err(error.context(continues)),
            (Err(error), None) => return Err(error),
        };
        Ok(ExportedSpace {
            manifest,
            removal: ended.removal,
            warnings,
        })
    }

    /// Write `manifest`'s package, a space of `repo` whose agents `settings`
    /// describes, with their session files to the new file `file`, and the
    /// warnings for what it left out.
    async fn write_package(
        &self,
        repo: &Repository,
        manifest: Manifest,
        settings: Result<Effective>,
        file: std::path::PathBuf,
    ) -> Result<(Manifest, Vec<String>)> {
        let (sessions, mut warnings) = self.session_sources(repo, settings, &manifest).await;
        let (manifest, left_out) = crate::transfer::export(repo, manifest, sessions, file).await?;
        warnings.extend(left_out);
        Ok((manifest, warnings))
    }

    /// Remove the workspace `selector` names for its export, or take the
    /// latest removal record with that name or ID when none exists.
    async fn remove_for_export(
        &self,
        selector: &str,
        unsaved: UnsavedWork,
        inspection: InspectionPolicy,
    ) -> Result<Ended> {
        let (removal, settings) = match self.workspace(selector).await {
            Ok(workspace) => {
                // Its own config may describe its agents, and goes with it.
                let settings = self.workspace_settings(&workspace).await;
                let removal = self
                    .remove_workspace(&workspace.id, unsaved, inspection)
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
                None => removed.workspace_id == selector || removed.name == selector,
            })
            .with_context(|| format!("no space or restorable workspace {selector}"))?;
        ensure!(
            !removed.checkpoint.files_omitted,
            "the removal could not keep the uncommitted files; shoal work restore {} opens the workspace here again",
            removed.name
        );
        let repo = self.repository(&removed.repository_id).await?;
        Ok(Ended {
            manifest: Manifest::of(&repo, Source::Workspace(removed)).await?,
            repo,
            removal: removal.map(|(_, removal)| removal),
            settings,
            continues: None,
        })
    }

    /// End the running `chat` for its export once its agent has reported
    /// the session the package continues and its package is made, so the
    /// session's files stop changing and only writing them is left.
    async fn end_chat_for_export(&self, chat: Chat) -> Result<Ended> {
        let Some(session) = chat.session.clone() else {
            anyhow::bail!(
                "the agent of chat {} reported no session to continue elsewhere, so the chat keeps running",
                chat.name
            );
        };
        let repo = self.repository(&chat.repository_id).await?;
        let continues = format!(
            "chat {name} ended here; shoal chat {repo} --name {name} --agent {agent} --session {session} continues it",
            name = chat.name,
            repo = crate::repository::name(&repo),
            agent = chat.agent,
        );
        let id = chat.id.clone();
        let manifest = Manifest::of(&repo, Source::Chat(chat)).await?;
        self.end_chat(&id).await?;
        Ok(Ended {
            repo,
            manifest,
            removal: None,
            settings: None,
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
        let home = self.paths.home.clone();
        let sessions = manifest.sessions.clone().unwrap_or_default();
        let found = tokio::task::spawn_blocking(move || {
            crate::cli::agents::session_sources(&settings, &home, &environments, &sessions)
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
