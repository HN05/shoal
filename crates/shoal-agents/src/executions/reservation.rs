//! The refusal of an execution in a workspace that a lifecycle operation
//! holds in a state that passes, which a start may wait out.
use anyhow::Result;
use rusqlite::Connection;
use shoal_ids::space_ids::SpaceId;
use shoal_store::lifecycle::{Lifecycle, lifecycle};

use crate::state::WorkspaceState;

/// The refusal of a workspace held in a lifecycle state that passes, which
/// a caller may wait out.
#[derive(Debug)]
pub struct Reserved(pub WorkspaceState);

impl std::fmt::Display for Reserved {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "workspace is {}", self.0)
    }
}

impl std::error::Error for Reserved {}

/// Refuse the workspace with [`Reserved`] while a lifecycle operation that
/// passes holds it.
pub fn ensure_unreserved(db: &Connection, id: &SpaceId) -> Result<()> {
    match lifecycle(db, id)? {
        Some(Lifecycle { state, .. }) if state.passes() => Err(Reserved(state).into()),
        _ => Ok(()),
    }
}
