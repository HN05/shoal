//! Spaces: workspaces and chats, which share one namespace of IDs and names,
//! so a request naming a space acts on whichever of them it names.
use anyhow::{Result, ensure};

use super::{
    workspace::Manager,
    workspace_files::{Attacher, NewFile},
};
use crate::model::{Completion, PresentSpace};

impl Manager {
    /// The workspace or chat `selector` names, by name or ID; the error is
    /// the workspace lookup's when neither has it.
    pub(crate) async fn present_space(&self, selector: &str) -> Result<PresentSpace> {
        let error = match self.workspace(selector).await {
            Ok(workspace) => return Ok(PresentSpace::Workspace(workspace)),
            Err(error) => error,
        };
        match self.recorded_chat(selector).await? {
            Some(chat) => Ok(PresentSpace::Chat(chat)),
            None => Err(error),
        }
    }

    /// Record the work of the workspace or running chat `selector` names as
    /// done, as [`Self::mark_done`] or [`Self::mark_chat_done`] does; only a
    /// workspace has the cleanup and branch `cleanup` and `integrated` are
    /// about.
    pub async fn mark_space_done(
        &self,
        selector: &str,
        cleanup: bool,
        integrated: bool,
        results: Vec<NewFile>,
        attacher: &Attacher,
    ) -> Result<Completion> {
        match self.present_space(selector).await? {
            PresentSpace::Workspace(workspace) => {
                (self.mark_done(&workspace.id, cleanup, integrated, results, attacher)).await
            }
            PresentSpace::Chat(chat) => {
                ensure!(
                    !cleanup && !integrated,
                    "a chat has no cleanup or branch; cleanup and integrated apply to workspaces"
                );
                self.mark_chat_done(&chat.id, results, attacher).await
            }
        }
    }

    /// Withdraw the completion the workspace or running chat `selector`
    /// names recorded.
    pub async fn undo_space_done(&self, selector: &str) -> Result<Completion> {
        match self.present_space(selector).await? {
            PresentSpace::Workspace(workspace) => self.undo_done(&workspace.id).await,
            PresentSpace::Chat(chat) => self.undo_chat_done(&chat.id).await,
        }
    }
}

mod export;
