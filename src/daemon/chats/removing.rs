//! The chats a removal has reserved: from before it removes the spaces below
//! a chat until it deletes the chat or gives up, nothing restores the chat,
//! so a removal that took its workspaces is not left with a running chat.
use std::{
    collections::HashSet,
    sync::{Mutex, PoisonError},
};

use anyhow::{Result, ensure};

/// The IDs of the chats being removed.
#[derive(Default)]
pub(crate) struct Removing(Mutex<HashSet<String>>);

impl Removing {
    /// Reserve the chat `id` for a removal until the returned reservation
    /// drops, refusing one another removal holds.
    pub(crate) fn reserve(&self, id: &str) -> Result<Reservation<'_>> {
        ensure!(
            self.lock().insert(id.to_owned()),
            "the chat is being removed"
        );
        Ok(Reservation {
            removing: self,
            id: id.to_owned(),
        })
    }

    /// Whether a removal holds the chat `id`.
    pub(super) fn reserved(&self, id: &str) -> bool {
        self.lock().contains(id)
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, HashSet<String>> {
        self.0.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

/// A chat a removal holds, until it drops.
pub(crate) struct Reservation<'a> {
    removing: &'a Removing,
    id: String,
}

impl Drop for Reservation<'_> {
    fn drop(&mut self) {
        self.removing.lock().remove(&self.id);
    }
}
