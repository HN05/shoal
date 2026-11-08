//! The running chats a stop is ending. Its rounds go on stopping what the
//! agent started after the agent exits, and those processes may use the
//! chat's temporary directory, so the chat keeps its record and directory
//! until the stop finishes, whoever finds its agent exited meanwhile.
use crate::client::space_ids::SpaceId;
use std::{
    collections::HashMap,
    sync::{Mutex, PoisonError},
};

/// How many stops of each chat are under way, by chat.
#[derive(Default)]
pub(crate) struct Stopping(Mutex<HashMap<SpaceId, usize>>);

impl Stopping {
    /// Keep the chat `id` from ending until the returned hold drops.
    pub(super) fn hold(&self, id: &SpaceId) -> Hold<'_> {
        *self.lock().entry(id.to_owned()).or_default() += 1;
        Hold {
            stopping: self,
            id: id.to_owned(),
        }
    }

    /// Whether a stop of the chat `id` is under way.
    pub(super) fn under_way(&self, id: &SpaceId) -> bool {
        self.lock().contains_key(id)
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, HashMap<SpaceId, usize>> {
        self.0.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

/// A stop of a chat under way, until it finishes.
pub(super) struct Hold<'a> {
    stopping: &'a Stopping,
    id: SpaceId,
}

impl Drop for Hold<'_> {
    fn drop(&mut self) {
        let mut stops = self.stopping.lock();
        if let Some(count) = stops.get_mut(&self.id) {
            *count -= 1;
            if *count == 0 {
                stops.remove(&self.id);
            }
        }
    }
}

#[cfg(test)]
mod tests;
