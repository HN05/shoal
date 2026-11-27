//! Converting a space into the other kind, and handing its work over to
//! another agent tool. See `docs/design/spaces.md`.
use anyhow::Result;
use rusqlite::Transaction;
use shoal_ids::space_ids::SpaceId;

use crate::{config::modes::ModeName, model::Workspace};

pub mod handoff;
pub mod handovers;
pub mod pending;
pub mod record;
pub mod relocation;
pub mod to_agent;
pub mod to_chat;
pub mod to_workspace;

pub use handovers::Handovers;
pub use pending::{Carrying, Pending};
pub use relocation::{ConversionStop, Relocating};
pub use to_workspace::WorkspaceConversion;

/// Also the mode the workspace's agent starts in and how it carries on.
#[derive(Debug, Clone)]
pub struct Converting {
    pub chat: crate::model::Chat,
    pub mode: ModeName,
    pub carrying: Carrying,
}

impl Converting {
    /// Takes the chat's ID, number and name, and the mode and permissions the
    /// chat's agent was given.
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

    /// Records that the chat's agent restarts in the workspace once its turn
    /// ends.
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
        pending::record(tx, &self.chat.id, &pending)
    }
}

/// `settings` are those of the space's repository. The mode must be defined
/// there and one a start may name.
pub fn ensure_mode(settings: &crate::config::Effective, mode: &ModeName) -> Result<()> {
    crate::config::modes::ensure_startable(mode)?;
    crate::config::modes::get(&settings.modes, mode).map(drop)
}

/// One whose agent has yet to start in the new place.
pub fn pending(db: &rusqlite::Connection, id: &SpaceId) -> Result<Option<Pending>> {
    pending::pending(db, id)
}
