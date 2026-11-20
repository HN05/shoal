//! What the scope a command runs in, in the work slice, records of its
//! memory: how many of its processes the kernel's OOM killer killed, and
//! the most memory it held. The wrapper reads both every [`INTERVAL`] while
//! the command runs and once more as it ends, but the scope goes with its
//! last process, as when the kernel kills all of a CI job's at once: a
//! command killed then whose scope is gone counts a kill when the slice,
//! whose count holds those of every scope in it, counted one since the
//! last read.
use std::{
    path::PathBuf,
    sync::{Arc, Mutex},
    time::Duration,
};

use shoal_core::process::{control_group, oom};
use shoal_protocol::protocol::ScopeMemory;

/// How often a running command's scope is read.
const INTERVAL: Duration = Duration::from_secs(1);

/// The exit code of a command the kernel killed, as SIGKILL ends it.
const KILLED: i32 = 128 + libc::SIGKILL;

/// A running command's scope, read in the background.
pub(super) struct Watch {
    read: Arc<Mutex<Read>>,
    task: tokio::task::JoinHandle<()>,
}

/// What the scope's reads found.
#[derive(Default)]
struct Read {
    /// The scope, once the command has moved into it.
    scope: Option<PathBuf>,
    memory: ScopeMemory,
    /// The slice's count of kills at the latest read.
    slice_kills: Option<u64>,
}

impl Watch {
    /// Read the scope of the command `pid`, this process's child, which it
    /// moves into once its launcher has started it.
    pub(super) fn start(pid: u32) -> Self {
        let read = Arc::new(Mutex::new(Read::default()));
        let reading = read.clone();
        let task = tokio::spawn(async move {
            let mut interval = tokio::time::interval(INTERVAL);
            loop {
                interval.tick().await;
                let mut read = reading.lock().expect("scope reads do not panic");
                read.find_scope(pid);
                read.update();
            }
        });
        Self { read, task }
    }

    /// What the scope recorded once the command ended with `code`; `None`
    /// where it never ran in a scope of its own.
    pub(super) fn finish(self, code: i32) -> Option<ScopeMemory> {
        self.task.abort();
        let mut read = self.read.lock().expect("scope reads do not panic");
        read.scope.as_ref()?;
        read.last(code, slice_count);
        Some(read.memory)
    }
}

/// Whether a command whose scope was gone at its last read was killed for
/// memory: it ended with SIGKILL, `code`, while the slice's count of kills
/// rose from `before`, at the previous read, to `now`.
fn killed_with_the_slice(code: i32, before: Option<u64>, now: Option<u64>) -> bool {
    code == KILLED && matches!((before, now), (Some(before), Some(now)) if now > before)
}

impl Read {
    /// Find the scope of the command `pid` until it has one apart from this
    /// process's.
    fn find_scope(&mut self, pid: u32) {
        if self.scope.is_none() {
            self.scope = oom::scope_of(pid).ok().flatten();
        }
    }

    /// Read the scope once more as its command ended with `code`: one gone
    /// by then counts a kill when `slice_count`, the slice's count of kills
    /// now, shows one since the previous read.
    fn last(&mut self, code: i32, slice_count: impl FnOnce() -> Option<u64>) {
        let before = self.slice_kills;
        if !self.update() && killed_with_the_slice(code, before, slice_count()) {
            self.memory.oom_kills = self.memory.oom_kills.saturating_add(1);
        }
    }

    /// Read the scope and the slice, returning whether the scope was there.
    fn update(&mut self) -> bool {
        let Some(scope) = &self.scope else {
            return false;
        };
        let (Ok(Some(kills)), Ok(held)) =
            (control_group::oom_kills(scope), control_group::held(scope))
        else {
            return false;
        };
        self.memory.oom_kills = kills;
        self.memory.peak = self.memory.peak.max(held.unwrap_or(0));
        self.slice_kills = slice_count();
        true
    }
}

/// The slice's count of kills, `None` where it cannot be read.
fn slice_count() -> Option<u64> {
    let slice = shoal_core::service::work::control_group()?;
    control_group::oom_kills(&slice).ok().flatten()
}

#[cfg(test)]
mod tests;
