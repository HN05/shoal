//! The chats a removal has reserved: from before it removes the spaces below
//! a chat until it deletes the chat or gives up, nothing restores the chat
//! and its agent asks no new question, so a removal that took its
//! workspaces is not left with a chat something keeps.
use crate::client::space_ids::SpaceId;
use std::{
    collections::HashSet,
    sync::{Arc, Mutex, PoisonError},
};

use anyhow::{Result, ensure};

/// The IDs of the chats being removed, shared with the store's thread, which
/// reads them in the transaction that records a question.
#[derive(Clone, Default)]
pub(crate) struct Removing(Arc<Mutex<HashSet<SpaceId>>>);

impl Removing {
    /// Reserve the chat `id` for a removal until the returned reservation
    /// drops, refusing one another removal holds.
    pub(crate) fn reserve(&self, id: &SpaceId) -> Result<Reservation<'_>> {
        ensure!(
            self.lock().insert(id.to_owned()),
            "another removal holds the chat"
        );
        Ok(Reservation {
            removing: self,
            id: id.to_owned(),
        })
    }

    /// Whether a removal holds the chat `id`.
    pub(crate) fn reserved(&self, id: &SpaceId) -> bool {
        self.lock().contains(id)
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, HashSet<SpaceId>> {
        self.0.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

/// A chat a removal holds, until it drops.
pub(crate) struct Reservation<'a> {
    removing: &'a Removing,
    id: SpaceId,
}

impl Drop for Reservation<'_> {
    fn drop(&mut self) {
        self.removing.lock().remove(&self.id);
    }
}
