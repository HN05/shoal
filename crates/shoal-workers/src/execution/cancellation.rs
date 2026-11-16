//! Cancelling this process's tracked commands, as an operation's worker
//! does once the daemon cancels its operation: a command running then, or
//! started later, stops as an interrupt stops it. A command still being
//! registered, such as setup behind a gated pre-setup hook, has no signal
//! listeners yet, so the cancellation outlasts the signal that caused it.
use std::sync::LazyLock;

use tokio::sync::watch;

static CANCELLED: LazyLock<watch::Sender<bool>> = LazyLock::new(|| watch::Sender::new(false));

pub fn cancel() {
    CANCELLED.send_replace(true);
}

/// Whether this process's commands were cancelled, and when they are.
pub fn cancellation() -> watch::Receiver<bool> {
    CANCELLED.subscribe()
}
