//! What a command that runs in no scope of its own, as on macOS, holds: the
//! wrapper reads its process tree every [`INTERVAL`], summing what each
//! process holds as the daemon's usage samples do, and keeps the peak,
//! which a sample every ten seconds would miss. On Linux the control group
//! it shares with the wrapper counts its OOM kills.
use std::{
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
    time::Duration,
};

use shoal_core::process::{control_group, oom, usage::tree_memory};
use shoal_protocol::protocol::ScopeMemory;
use tokio::sync::watch;

/// How often the command's tree is read.
const INTERVAL: Duration = Duration::from_secs(1);

/// A running command's tree, read in the background.
pub(super) struct Watch {
    peak: Arc<Mutex<u64>>,
    /// The control group the command runs in, with its count of OOM kills
    /// when the command started; none where it cannot be read.
    kills: Option<(PathBuf, u64)>,
    task: tokio::task::JoinHandle<()>,
}

/// A watch dropped unfinished, as when its command's registration fails,
/// reads no more.
impl Drop for Watch {
    fn drop(&mut self) {
        self.task.abort();
    }
}

impl Watch {
    /// Read the tree of the command `pid`, telling `using` what it holds
    /// after each read.
    pub(super) fn start(pid: u32, using: watch::Sender<u64>) -> Self {
        let group = oom::group_of(pid).ok().flatten();
        Self::start_in(pid, group, using)
    }

    /// [`Self::start`] for the command `pid` in the control group `group`.
    fn start_in(pid: u32, group: Option<PathBuf>, using: watch::Sender<u64>) -> Self {
        let kills = group.and_then(|group| {
            let before = oom_kills(&group)?;
            Some((group, before))
        });
        let peak = Arc::new(Mutex::new(0));
        let reading = peak.clone();
        let task = tokio::spawn(async move {
            let mut interval = tokio::time::interval(INTERVAL);
            loop {
                interval.tick().await;
                let read = tokio::task::spawn_blocking(move || tree_memory(pid)).await;
                let Ok(Ok(memory)) = read else {
                    continue;
                };
                let mut peak = reading.lock().expect("tree reads do not panic");
                *peak = (*peak).max(memory);
                using.send_replace(memory);
            }
        });
        Self { peak, kills, task }
    }

    /// The most memory the command's tree held while it ran, and the OOM
    /// kills in its control group meanwhile. Others share that group, so a
    /// kill of one of them counts too, which makes the next estimate err
    /// high.
    pub(super) fn finish(self) -> ScopeMemory {
        self.task.abort();
        let oom_kills = (self.kills.as_ref())
            .and_then(|(group, before)| Some(oom_kills(group)?.saturating_sub(*before)));
        ScopeMemory {
            oom_kills: oom_kills.unwrap_or(0),
            peak: *self.peak.lock().expect("tree reads do not panic"),
        }
    }
}

/// How many processes of `group` any OOM killer killed, where it reads.
fn oom_kills(group: &Path) -> Option<u64> {
    control_group::oom_kills(group).ok().flatten()
}

#[cfg(test)]
mod tests;
