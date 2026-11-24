//! Converting a chat into a workspace and a workspace into a chat, and
//! handing a space's work over to another agent tool: the space keeps its
//! ID, number, name, place in the tree, links, threads, files and
//! progress, and its agent carries on with its conversation in the new
//! place, or the new tool from copies of it, once its current turn ends.
use anyhow::Result;
use rusqlite::Transaction;
use shoal_ids::space_ids::SpaceId;

use crate::{config::modes::ModeName, model::Workspace};

pub mod handoff;
pub mod handovers;
pub mod record;
pub mod relocation;
pub mod to_agent;
pub mod to_chat;
pub mod to_workspace;

pub use handovers::Handovers;
pub use record::{Carrying, Pending};
pub use relocation::{ConversionStop, Relocating};
pub use to_workspace::WorkspaceConversion;

/// The chat a new workspace replaces, the mode the workspace's agent
/// starts in and how it carries on.
#[derive(Debug, Clone)]
pub struct Converting {
    pub chat: crate::model::Chat,
    pub mode: ModeName,
    pub carrying: Carrying,
}

impl Converting {
    /// The new workspace `record` under the chat's ID, number and name, in
    /// the mode and with the permissions the chat's agent was given.
    pub fn workspace(&self, record: Workspace) -> Workspace {
        Workspace {
            id: self.chat.id.clone(),
            number: self.chat.number,
            name: self.chat.name.clone(),
            mode: Some(self.mode.clone()),
            narrowing: self.chat.narrowing,
            ..record
        }
    }

    /// Replace the chat with the workspace just recorded under its ID, and
    /// record that the chat's agent restarts there once its turn ends.
    pub fn replace_chat(&self, tx: &Transaction<'_>) -> Result<()> {
        let switches = self.carrying.taking.is_some();
        let agent = (&self.mode, &self.chat.narrowing);
        let (chat, execution) = record::replace_chat(tx, &self.chat.id, agent, switches)?;
        let pending = Pending::Workspace {
            mode: self.mode.clone(),
            chat,
            execution,
            preparation: None,
            prepared: false,
            hook_failure: None,
            carrying: self.carrying.clone(),
        };
        record::record(tx, &self.chat.id, &pending)
    }
}

/// Refuse `mode` for the agent of a converted space, unless `settings`, those
/// of the space's repository, define it and a start may name it.
pub fn ensure_mode(settings: &crate::config::Effective, mode: &ModeName) -> Result<()> {
    crate::config::modes::ensure_startable(mode)?;
    crate::config::modes::get(&settings.modes, mode).map(drop)
}

/// The conversion of the space `id` whose agent has yet to start in the new
/// place.
pub fn pending(db: &rusqlite::Connection, id: &SpaceId) -> Result<Option<Pending>> {
    record::pending(db, id)
}
