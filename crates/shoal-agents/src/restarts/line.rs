//! The line sequential restarts take on this machine: one at a time, each
//! at least the admission stagger after the previous start, in the order
//! they joined it.
use std::{collections::VecDeque, sync::Arc};

use shoal_ids::space_ids::SpaceId;
use tokio::{sync::Notify, time::Instant};

use crate::Agents;
use crate::capacity::STAGGER;
use crate::daemon::Daemon;

/// The spaces whose recorded restarts wait in the line.
#[derive(Default)]
pub struct Line {
    queued: std::sync::Mutex<VecDeque<SpaceId>>,
    joined: Notify,
}

impl Line {
    /// Put the restart recorded for the space `id` at the end of the line,
    /// unless it waits there already.
    pub fn join(&self, id: &SpaceId) {
        let mut queued = self.queued.lock().unwrap_or_else(|e| e.into_inner());
        if !queued.contains(id) {
            queued.push_back(id.clone());
            self.joined.notify_one();
        }
    }

    /// The space whose restart is next, once one waits.
    async fn next(&self) -> SpaceId {
        loop {
            let first = (self.queued.lock().unwrap_or_else(|e| e.into_inner())).pop_front();
            if let Some(id) = first {
                return id;
            }
            self.joined.notified().await;
        }
    }
}

/// Restart the agents of the line in turn, as long as the daemon runs.
pub async fn run(agents: Arc<Agents>, daemon: Arc<dyn Daemon>) {
    let mut previous: Option<Instant> = None;
    loop {
        let id = agents.restarts.line.next().await;
        if let Some(previous) = previous {
            tokio::time::sleep_until(previous + STAGGER).await;
        }
        if agents.act_on_restart(&*daemon, &id).await {
            previous = Some(Instant::now());
        }
    }
}

#[cfg(test)]
mod tests;
