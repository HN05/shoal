//! Packages of spaces for `shoal move`: the daemon ends a space here, a
//! workspace by its removal and a chat by stopping its agent, and writes
//! what continues it on another machine.
use anyhow::{Context, Result, ensure};

use super::PresentSpace;
use crate::{
    config::Effective,
    daemon::workspace::Manager,
    model::{Chat, Repository},
    removal::{InspectionPolicy, RemovalResult, UnsavedWork},
    transfer::{ExportedSpace, Manifest, SessionSource, Source},
};

/// A space ended here for its package: what the package is made from, the
/// removal that ended a workspace, and the settings describing its agents
/// when they are not the repository's.
struct Ended {
    source: Source,
    removal: Option<RemovalResult>,
    settings: Option<Result<Effective>>,
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
        let repo = self.repository(ended.source.repository_id()).await?;
        let settings = match ended.settings {
            Some(settings) => settings,
            None => self.repository_settings(&repo.id).await,
        };
        let manifest = Manifest::of(&repo, ended.source).await?;
        let (sessions, mut warnings) = self.session_sources(&repo, settings, &manifest).await;
        let (manifest, left_out) = crate::transfer::export(&repo, manifest, sessions, file).await?;
        warnings.extend(left_out);
        Ok(ExportedSpace {
            manifest,
            removal: ended.removal,
            warnings,
        })
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
        Ok(Ended {
            source: Source::Workspace(removed),
            removal: removal.map(|(_, removal)| removal),
            settings,
        })
    }

    /// End the running `chat` for its export once its agent has reported
    /// the session the package continues, so the session's files stop
    /// changing.
    async fn end_chat_for_export(&self, chat: Chat) -> Result<Ended> {
        ensure!(
            chat.session.is_some(),
            "the agent of chat {} reported no session to continue elsewhere, so the chat keeps running",
            chat.name
        );
        self.end_chat(&chat.id).await?;
        Ok(Ended {
            source: Source::Chat(chat),
            removal: None,
            settings: None,
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
