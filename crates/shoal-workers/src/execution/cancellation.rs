//! Cancelling this process's tracked commands once the daemon cancels its
//! operation. A command still being registered has no signal listeners
//! yet, so the cancellation outlasts the signal that caused it.
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
