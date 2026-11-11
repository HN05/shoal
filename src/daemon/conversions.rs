//! Converting a chat into a workspace and a workspace into a chat: the space
//! keeps its ID, number, name, place in the tree, links, questions, files
//! and progress, and its agent carries on with its conversation in the new
//! place once its current turn ends.
use crate::client::space_ids::SpaceId;
use anyhow::Result;
use rusqlite::Transaction;

use crate::{config::modes::ModeName, model::Workspace};

pub(crate) mod record;
mod relocation;
mod to_chat;
mod to_workspace;

pub(crate) use record::Pending;
pub(crate) use relocation::run;
pub(crate) use to_workspace::WorkspaceConversion;

/// The chat a new workspace replaces, and the mode the workspace's agent
/// starts in.
#[derive(Debug, Clone)]
pub(crate) struct Converting {
    pub chat: crate::model::Chat,
    pub mode: ModeName,
}

impl Converting {
    /// The new workspace `record` under the chat's ID, number and name, in
    /// the mode and with the permissions the chat's agent was given.
    pub(crate) fn workspace(&self, record: Workspace) -> Workspace {
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
    pub(crate) fn replace_chat(&self, tx: &Transaction<'_>) -> Result<()> {
        let chat = record::replace_chat(tx, &self.chat.id)?;
        let pending = Pending::Workspace {
            mode: self.mode.clone(),
            chat,
        };
        record::record(tx, &self.chat.id, &pending)
    }
}

/// Refuse `mode` for the agent of a converted space, unless `settings`, those
/// of the space's repository, define it and a start may name it.
pub(super) fn ensure_mode(settings: &crate::config::Effective, mode: &ModeName) -> Result<()> {
    crate::config::modes::ensure_startable(mode)?;
    crate::config::modes::get(&settings.modes, mode).map(drop)
}

/// The conversion of the space `id` whose agent has yet to start in the new
/// place.
pub(crate) fn pending(db: &rusqlite::Connection, id: &SpaceId) -> Result<Option<Pending>> {
    record::pending(db, id)
}
