use std::{
    thread,
    time::{Duration, Instant},
};

pub fn wait_until(what: &str, mut done: impl FnMut() -> bool) {
    wait_for(what, || done().then_some(()));
}

pub fn wait_for<T>(what: &str, poll: impl FnMut() -> Option<T>) -> T {
    try_wait_for(poll).unwrap_or_else(|| panic!("timed out waiting for {what}"))
}

/// Return on timeout so callers can clean up and report additional diagnostics.
pub fn try_wait_for<T>(poll: impl FnMut() -> Option<T>) -> Option<T> {
    try_wait_within(super::HANG_GUARD, poll)
}

/// `try_wait_for` for work that itself waits, with hang guards of its own,
/// that the caller's must outlast.
pub fn try_wait_within<T>(limit: Duration, mut poll: impl FnMut() -> Option<T>) -> Option<T> {
    // A hang guard only: test outcomes must not depend on runner speed.
    let deadline = Instant::now() + limit;
    loop {
        if let Some(value) = poll() {
            return Some(value);
        }
        if Instant::now() >= deadline {
            return None;
        }
        thread::sleep(Duration::from_millis(20));
    }
}
