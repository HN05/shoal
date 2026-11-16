//! Triggers, the daemon's side of `[triggers.NAME]`: it reads the
//! configured triggers, keeps each one's runtime state, and fires them, the
//! timer source's from a background loop and any trigger at once for
//! `shoal trigger run`. Every firing takes the same steps whatever its
//! source, and launches through the operation `shoal start` uses.
use tokio::sync::{Mutex, Notify};

mod commands;
mod configured;
mod firing;
mod state;
mod timer;

pub(crate) use configured::Configured;
pub(crate) use timer::run;

/// What the triggers' background loop shares with requests.
#[derive(Default)]
pub(crate) struct Triggers {
    /// Wakes the loop for a changed config or runtime state.
    wake: Notify,
    /// Held through each firing and each pass of the loop, so a trigger
    /// never fires twice for one time.
    firing: Mutex<()>,
}

impl Triggers {
    /// Have the loop read the triggers and their state again now.
    pub(crate) fn wake(&self) {
        self.wake.notify_one();
    }
}

#[cfg(test)]
mod tests;
