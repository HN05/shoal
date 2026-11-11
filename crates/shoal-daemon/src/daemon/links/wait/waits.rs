//! The registry of running waits, one per workspace or chat, in which a
//! newer wait supersedes an older one.
use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
    time::Duration,
};

use anyhow::Result;
use tokio::{sync::Notify, time::Instant};

use crate::client::space_ids::SpaceId;

/// The running wait of each workspace or chat, identified by its
/// supersession signal. Waits share one cursor per item, so a newer wait
/// supersedes the older one instead of competing for its updates.
#[derive(Clone)]
pub(crate) struct ActiveWaits {
    running: Arc<Mutex<HashMap<SpaceId, Arc<Notify>>>>,
    /// When the last wait of each space ended.
    ended: Arc<Mutex<HashMap<SpaceId, Instant>>>,
    /// When the daemon began tracking waits, for a space none ran in since.
    since: Instant,
}

impl Default for ActiveWaits {
    fn default() -> Self {
        Self {
            running: Arc::default(),
            ended: Arc::default(),
            since: Instant::now(),
        }
    }
}

pub(crate) struct ActiveWait<'a> {
    waits: &'a ActiveWaits,
    workspace: SpaceId,
    pub(crate) superseded: Arc<Notify>,
}

impl ActiveWaits {
    pub(crate) fn start(&self, workspace: &SpaceId) -> ActiveWait<'_> {
        let superseded = Arc::new(Notify::new());
        let older = self
            .running
            .lock()
            .expect("active waits lock")
            .insert(workspace.to_owned(), superseded.clone());
        if let Some(older) = older {
            older.notify_one();
        }
        ActiveWait {
            waits: self,
            workspace: workspace.to_owned(),
            superseded,
        }
    }

    /// Whether no wait has run for the space `key` for `delay`.
    pub(super) fn unwatched_for(&self, key: &SpaceId, delay: Duration) -> bool {
        let running = self.running.lock().expect("active waits lock");
        if running.contains_key(key) {
            return false;
        }
        let ended = self.ended.lock().expect("active waits lock");
        ended.get(key).copied().unwrap_or(self.since).elapsed() >= delay
    }
}

impl ActiveWaits {
    /// Run `operation` with the space at the end of each of `paths` that no
    /// wait of `registries`, the workspaces' then the chats', runs in
    /// anywhere along its path, since a wait there receives its updates too,
    /// whatever the kind of the space it runs in; no wait can start in
    /// either while it runs. Every caller locks them in that order.
    pub(super) fn with_unwatched(
        [workspaces, chats]: [&Self; 2],
        paths: &[Vec<SpaceId>],
        operation: impl FnOnce(&[SpaceId]) -> Result<()>,
    ) -> Result<()> {
        let workspaces = workspaces.running.lock().expect("active waits lock");
        let chats = chats.running.lock().expect("active waits lock");
        let waiting = |space: &SpaceId| workspaces.contains_key(space) || chats.contains_key(space);
        let unwatched = paths
            .iter()
            .filter(|path| !path.iter().any(waiting))
            .filter_map(|path| path.last().cloned())
            .collect::<Vec<_>>();
        operation(&unwatched)
    }
}

#[cfg(test)]
impl ActiveWaits {
    pub(crate) fn is_waiting(&self, key: &SpaceId) -> bool {
        self.running
            .lock()
            .expect("active waits lock")
            .contains_key(key)
    }
}

impl ActiveWait<'_> {
    fn is_current_in(&self, waits: &HashMap<SpaceId, Arc<Notify>>) -> bool {
        waits
            .get(&self.workspace)
            .is_some_and(|current| Arc::ptr_eq(current, &self.superseded))
    }

    pub(crate) fn is_current(&self) -> bool {
        self.is_current_in(&self.waits.running.lock().expect("active waits lock"))
    }
}

impl Drop for ActiveWait<'_> {
    fn drop(&mut self) {
        let mut running = self.waits.running.lock().expect("active waits lock");
        if self.is_current_in(&running) {
            running.remove(&self.workspace);
            let mut ended = self.waits.ended.lock().expect("active waits lock");
            ended.insert(self.workspace.clone(), Instant::now());
        }
    }
}

#[cfg(test)]
mod tests;
