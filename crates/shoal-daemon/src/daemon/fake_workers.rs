//! Whether a daemon opened from now on is a test daemon, whose operations
//! start fake workers that tests drive instead of running this binary as
//! them, and which discovers none of the machine's terminal hosts: always
//! in the daemon's own tests, and in another crate's once it starts a
//! `TestDaemon` or the overload fixture. The switch is read at run time
//! because a workspace test build gives the `shoal` binary the
//! `test-support` feature too, and the binary starts real workers and reads
//! real hosts.
use std::sync::atomic::{AtomicBool, Ordering};

static FAKED: AtomicBool = AtomicBool::new(cfg!(test));

pub(crate) fn faked() -> bool {
    FAKED.load(Ordering::Relaxed)
}

/// Act as a test daemon from now on, in this process.
pub(crate) fn fake() {
    FAKED.store(true, Ordering::Relaxed);
}
