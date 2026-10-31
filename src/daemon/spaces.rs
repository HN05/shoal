//! Spaces: workspaces and chats, which share one namespace of IDs and names,
//! so a request naming a space acts on whichever of them it names.
use anyhow::Result;

use super::workspace::Manager;
use crate::model::{Chat, Workspace};

/// The workspace or running chat a selector names.
#[derive(Debug, Clone)]
#[allow(
    clippy::large_enum_variant,
    reason = "a lookup's result, read once by the request that made it"
)]
pub(crate) enum PresentSpace {
    Workspace(Workspace),
    Chat(Chat),
}

impl Manager {
    /// The workspace or running chat `selector` names, by name or ID; the
    /// error is the workspace lookup's when neither has it.
    pub(crate) async fn present_space(&self, selector: &str) -> Result<PresentSpace> {
        let error = match self.workspace(selector).await {
            Ok(workspace) => return Ok(PresentSpace::Workspace(workspace)),
            Err(error) => error,
        };
        match self.running_chat(selector).await? {
            Some(chat) => Ok(PresentSpace::Chat(chat)),
            None => Err(error),
        }
    }
}

mod export;
