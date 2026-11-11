//! Whether sustained healthy readings let protection-stopped agents restore.
//! Each stop, reload or unsafe reading starts a new epoch, so readings taken
//! before it no longer admit a restore.
use std::sync::atomic::{AtomicU64, Ordering};

use tokio::sync::watch;

#[derive(Default)]
pub(crate) struct OverloadRecovery {
    /// The epoch the overload monitor last found sustained healthy readings
    /// in, if its latest readings were.
    ready: watch::Sender<Option<u64>>,
    epoch: AtomicU64,
}

impl OverloadRecovery {
    /// The current epoch.
    pub(crate) fn epoch(&self) -> u64 {
        self.epoch.load(Ordering::SeqCst)
    }

    /// Start a new epoch, withdrawing the readings published so far.
    pub(crate) fn reset(&self) {
        self.epoch.fetch_add(1, Ordering::SeqCst);
        self.ready.send_replace(None);
    }

    /// Publish whether the monitor's readings in `epoch` were sustained and
    /// healthy.
    pub(crate) fn publish(&self, ready: Option<u64>) {
        self.ready.send_replace(ready);
    }

    pub(crate) fn watch(&self) -> watch::Receiver<Option<u64>> {
        self.ready.subscribe()
    }

    /// The epoch the overload monitor published sustained healthy readings
    /// for, if it still holds.
    pub(crate) fn published(&self) -> Option<u64> {
        let epoch = self.epoch();
        (*self.ready.borrow() == Some(epoch)).then_some(epoch)
    }

    /// Consume the healthy interval published for `epoch`, returning the
    /// epoch a restore admitted on it holds; `None` when a stop, reload or
    /// unsafe reading advanced the epoch first.
    pub(crate) fn consume(&self, epoch: u64) -> Option<u64> {
        self.epoch
            .compare_exchange(epoch, epoch + 1, Ordering::SeqCst, Ordering::SeqCst)
            .ok()?;
        self.ready.send_replace(None);
        Some(epoch + 1)
    }
}
