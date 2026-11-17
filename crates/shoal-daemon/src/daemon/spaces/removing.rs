//! The spaces a removal has reserved: from before it removes the spaces
//! below a space until it deletes the space or gives up, nothing restores its
//! agent, opens a space below it or records its agent's new question, so a
//! removal that took the spaces below is not left with a space something
//! keeps.
use shoal_ids::space_ids::SpaceId;
use std::{
    collections::HashSet,
    sync::{Arc, Mutex, PoisonError},
};

use anyhow::{Result, ensure};
use rusqlite::Connection;

use crate::{daemon::workspace::Manager, model::Space, state::WorkspaceState};

impl Manager {
    /// Whether `space` is being removed: a removal holds it, or its
    /// worktree's lifecycle records one.
    pub(crate) fn being_removed(&self, space: &Space) -> bool {
        self.space_removals.reserved(&space.id)
            || space
                .worktree()
                .is_some_and(|worktree| worktree.state == WorkspaceState::Removing)
    }
}

/// The IDs of the spaces being removed, shared with the store's thread, which
/// reads them in the transactions that record what a reserved space refuses.
#[derive(Clone, Default)]
pub(crate) struct Removing(Arc<Mutex<HashSet<SpaceId>>>);

impl Removing {
    /// Reserve the space `id` for a removal until the returned reservation
    /// drops, refusing one another removal holds.
    pub(crate) fn reserve(&self, id: &SpaceId) -> Result<Reservation<'_>> {
        ensure!(
            self.lock().insert(id.to_owned()),
            "the space is already being removed; it will disappear shortly"
        );
        Ok(Reservation {
            removing: self,
            id: id.to_owned(),
        })
    }

    /// Whether a removal holds the space `id`.
    pub(crate) fn reserved(&self, id: &SpaceId) -> bool {
        self.lock().contains(id)
    }

    /// Refuse new work for the space `id`, such as a hold, a question or a
    /// launch it queues, while a removal holds it, and for a workspace while
    /// its worktree is not ready. A reservation precedes the removal's next
    /// read on the store's one thread, so that read sees what this lets
    /// through.
    pub(crate) fn require_taking_work(&self, db: &Connection, id: &SpaceId) -> Result<()> {
        ensure!(!self.reserved(id), "the space is being removed");
        crate::daemon::links::space::require_ready_space(db, id)
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, HashSet<SpaceId>> {
        self.0.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

/// A space a removal holds, until it drops.
pub(crate) struct Reservation<'a> {
    removing: &'a Removing,
    id: SpaceId,
}

impl Drop for Reservation<'_> {
    fn drop(&mut self) {
        self.removing.lock().remove(&self.id);
    }
}
