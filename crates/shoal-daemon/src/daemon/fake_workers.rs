//! Whether the daemon is a test daemon, which starts fake workers that
//! tests drive instead of running this binary as them, and discovers none of
//! the machine's terminal hosts: always in the daemon's own tests, and in
//! another crate's once it starts a `TestDaemon` or the overload fixture. The
//! switch is read at run time because a workspace test build gives the
//! `shoal` binary the `test-support` feature too, and the binary starts real
//! workers and reads real hosts.
use std::sync::atomic::{AtomicBool, Ordering};

static FAKED: AtomicBool = AtomicBool::new(cfg!(test));

pub(crate) fn faked() -> bool {
    FAKED.load(Ordering::Relaxed)
}

/// Act as a test daemon from now on, in this process.
#[cfg(any(test, feature = "test-support"))]
pub(crate) fn fake() {
    FAKED.store(true, Ordering::Relaxed);
}
