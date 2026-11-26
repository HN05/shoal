//! Whether a daemon opened from now on is a test daemon, which starts fake
//! workers and discovers none of the machine's terminal hosts. The switch
//! is read at run time, not from the `test-support` feature: a workspace
//! test build gives the `shoal` binary that feature too, and the binary
//! must start real workers and read real hosts.
use std::sync::atomic::{AtomicBool, Ordering};

static FAKED: AtomicBool = AtomicBool::new(cfg!(test));

pub(crate) fn faked() -> bool {
    FAKED.load(Ordering::Relaxed)
}

/// Act as a test daemon from now on, in this process.
pub(crate) fn fake() {
    FAKED.store(true, Ordering::Relaxed);
}
