//! Whether the daemon starts fake workers, which tests drive, instead of
//! running this binary as them: always in the daemon's own tests, and in
//! another crate's once it starts a `TestDaemon`. The switch is read at run
//! time because a workspace test build gives the `shoal` binary the
//! `test-support` feature too, and the binary starts real workers.
use std::sync::atomic::{AtomicBool, Ordering};

static FAKED: AtomicBool = AtomicBool::new(cfg!(test));

pub(crate) fn faked() -> bool {
    FAKED.load(Ordering::Relaxed)
}

/// Start fake workers from now on, in this process.
pub(crate) fn fake() {
    FAKED.store(true, Ordering::Relaxed);
}
