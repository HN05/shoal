//! When polls of item activity began, and the reads watches started from.
use std::{
    collections::HashMap,
    sync::{
        Mutex,
        atomic::{AtomicU64, Ordering},
    },
};

use shoal_ids::space_ids::SpaceId;
use shoal_protocol::time::UnixTime;

use crate::Links;

/// When a poll of item activity began: its time, which the space's own
/// changes are compared with, and its place among the daemon's reads,
/// which tells it from a watch's start read made in the same second.
#[derive(Clone, Copy)]
pub struct PollStart {
    pub at: UnixTime,
    read: u64,
}

impl PollStart {
    /// Its place among the daemon's reads.
    pub fn read(self) -> u64 {
        self.read
    }
}

/// The daemon's reads of items for watches, numbered in the order they
/// began, and the read each watch last started from. A read in flight
/// ends with the daemon, so none of this outlives it.
#[derive(Default)]
pub struct WatchStarts {
    reads: AtomicU64,
    started: Mutex<HashMap<(SpaceId, String), u64>>,
}

impl WatchStarts {
    /// The number of a read beginning now.
    fn begin(&self) -> u64 {
        self.reads.fetch_add(1, Ordering::SeqCst)
    }

    /// Note that the watches of the space `id` of `urls` started from `read`.
    pub fn started(&self, id: &SpaceId, urls: Vec<String>, read: u64) {
        let mut started = self
            .started
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        for url in urls {
            started.insert((id.to_owned(), url), read);
        }
    }

    /// Which of `urls` the space `id` started watching from a read that
    /// began after `read`, so a poll that began at `read` may show them
    /// before it.
    pub fn stale<'a>(
        &self,
        id: &SpaceId,
        urls: impl Iterator<Item = &'a String>,
        read: u64,
    ) -> Vec<String> {
        let started = self
            .started
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        urls.filter(|url| {
            started
                .get(&(id.to_owned(), (*url).clone()))
                .is_some_and(|start| *start > read)
        })
        .cloned()
        .collect()
    }
}

impl Links {
    /// The start of a poll of item activity beginning now.
    pub fn poll_start(&self) -> PollStart {
        self.poll_start_at(UnixTime::now())
    }

    /// The start of a poll beginning now as if at `at`.
    pub fn poll_start_at(&self, at: UnixTime) -> PollStart {
        PollStart {
            at,
            read: self.watch_starts.begin(),
        }
    }

    /// The daemon's reads of items for watches, and the read each watch
    /// last started from.
    pub fn watch_starts(&self) -> &WatchStarts {
        &self.watch_starts
    }
}
