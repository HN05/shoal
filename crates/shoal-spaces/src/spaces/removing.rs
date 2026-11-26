//! The spaces a removal has reserved. From before it removes the spaces
//! below until it deletes the space or gives up, nothing restores the
//! space's agent, opens a space below it or records a new thread it opens,
//! so the removal is not left with a space something keeps.
use crate::Spaces;
use shoal_ids::space_ids::SpaceId;
use std::{
    collections::HashSet,
    sync::{Arc, Mutex, PoisonError},
};

use anyhow::{Result, ensure};
use rusqlite::Connection;

use crate::{model::Space, state::WorkspaceState};

impl Spaces {
    /// A removal holds it, or its worktree's lifecycle records one.
    pub fn being_removed(&self, space: &Space) -> bool {
        self.space_removals.reserved(&space.id)
            || space
                .worktree()
                .is_some_and(|worktree| worktree.state == WorkspaceState::Removing)
    }
}

/// Shared with the store's thread, which reads them in the transactions
/// that record what a reserved space refuses.
#[derive(Clone, Default)]
pub struct Removing(Arc<Mutex<HashSet<SpaceId>>>);

impl Removing {
    /// Holds until the returned reservation drops. Refuses a space another
    /// removal holds.
    pub fn reserve(&self, id: &SpaceId) -> Result<Reservation<'_>> {
        ensure!(
            self.lock().insert(id.to_owned()),
            "the space is already being removed; it will disappear shortly"
        );
        Ok(Reservation {
            removing: self,
            id: id.to_owned(),
        })
    }

    pub fn reserved(&self, id: &SpaceId) -> bool {
        self.lock().contains(id)
    }

    /// Refuses new work, such as a hold, a thread or a launch, while a
    /// removal holds the space, and for a workspace while its worktree is
    /// not ready. A reservation precedes the removal's next read on the
    /// store's one thread, so that read sees what this lets through.
    pub fn require_taking_work(&self, db: &Connection, id: &SpaceId) -> Result<()> {
        ensure!(!self.reserved(id), "the space is being removed");
        crate::store::require_ready_space(db, id)
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, HashSet<SpaceId>> {
        self.0.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

pub struct Reservation<'a> {
    removing: &'a Removing,
    id: SpaceId,
}

impl Drop for Reservation<'_> {
    fn drop(&mut self) {
        self.removing.lock().remove(&self.id);
    }
}
