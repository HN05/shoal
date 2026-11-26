//! What the scope a command runs in records of its memory. The scope goes
//! with its last process, so a kill whose scope is gone counts by the work
//! slice's count instead.
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

/// The slice's count of kills before a command starts, which a kill of the
/// command before its scope is found shows against.
pub(super) struct Baseline {
    slice_kills: Option<u64>,
}

impl Baseline {
    /// The slice's count of kills now, before the command starts.
    pub(super) fn read() -> Self {
        Self {
            slice_kills: slice_count(),
        }
    }

    /// Read the scope of the command `pid`, this process's child, which it
    /// moves into once its launcher has started it.
    pub(super) fn watch(self, pid: u32) -> Watch {
        let read = Read {
            slice_kills: self.slice_kills,
            ..Read::default()
        };
        Watch::start(pid, read)
    }
}

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

/// A watch dropped unfinished, as when its command's registration fails,
/// reads no more.
impl Drop for Watch {
    fn drop(&mut self) {
        self.task.abort();
    }
}

impl Watch {
    /// Read the scope of the command `pid` from `read`, the reads so far.
    fn start(pid: u32, read: Read) -> Self {
        let read = Arc::new(Mutex::new(read));
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
    /// where it never ran in a scope of its own and no kill shows.
    pub(super) fn finish(self, code: i32) -> Option<ScopeMemory> {
        self.task.abort();
        let mut read = self.read.lock().expect("scope reads do not panic");
        read.last(code, slice_count);
        read.recorded()
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
    /// by then, or never found, counts a kill when `slice_count`, the
    /// slice's count of kills now, shows one since the previous read.
    fn last(&mut self, code: i32, slice_count: impl FnOnce() -> Option<u64>) {
        let before = self.slice_kills;
        if !self.update() && killed_with_the_slice(code, before, slice_count()) {
            self.memory.oom_kills = self.memory.oom_kills.saturating_add(1);
        }
    }

    /// What the reads recorded: nothing for a command found in no scope of
    /// its own, unless it was killed before it was found.
    fn recorded(&self) -> Option<ScopeMemory> {
        (self.scope.is_some() || self.memory.oom_kills > 0).then_some(self.memory)
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
