//! What the daemon keeps of the issues, PRs and CI runs spaces link until
//! it moves into `shoal-links`, and whether a space takes link changes.
mod actions;
pub(in crate::daemon) mod branches;
mod ci;
mod forges;
pub mod issue;
mod listing;
mod merge_path;
mod merge_review;
mod merge_warning;
mod permissions;
pub mod pr;
pub mod proposals;
mod pushes;
mod raw;
mod reads;
pub(crate) mod records;
mod revert;
mod run_approval;
mod runs;
pub(crate) mod space;
mod spaces;
pub(in crate::daemon) mod stack;
mod verdict;
mod view;
pub mod wait;

use anyhow::{Result, ensure};
use rusqlite::{Connection, OptionalExtension};
use shoal_ids::space_ids::SpaceId;
use shoal_links::space::LinkSpace;

use crate::state::WorkspaceState;

/// Refuse changes while the workspace is removed or otherwise in transition,
/// so a removal record taken during removal holds its final links; a chat
/// takes them whenever it runs.
pub(crate) fn ensure_accepts_links(db: &Connection, id: &SpaceId) -> Result<()> {
    if space::is_chat(db, id)? {
        return Ok(());
    }
    let state = state(db, id)?;
    ensure!(
        state.accepts_links(),
        "workspace cannot change links while {state}"
    );
    Ok(())
}

/// Whether the workspace still exists and its state takes link changes now,
/// or the chat still runs.
pub(crate) fn accepts_links(db: &Connection, id: &SpaceId) -> Result<bool> {
    if space::is_chat(db, id)? {
        return Ok(true);
    }
    let state: Option<WorkspaceState> = db
        .query_row("SELECT state FROM workspaces WHERE id=?1", [id], |row| {
            row.get(0)
        })
        .optional()?;
    Ok(state.is_some_and(WorkspaceState::accepts_links))
}

fn state(db: &Connection, id: &SpaceId) -> Result<WorkspaceState> {
    Ok(
        db.query_row("SELECT state FROM workspaces WHERE id=?1", [id], |row| {
            row.get(0)
        })?,
    )
}

#[cfg(test)]
mod tests;
