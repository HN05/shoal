//! What a command that runs in no scope of its own, as on macOS, holds: the
//! wrapper reads its process tree every [`INTERVAL`], summing what each
//! process holds as the daemon's usage samples do, and keeps the peak,
//! which a sample every ten seconds would miss.
use std::{
    sync::{Arc, Mutex},
    time::Duration,
};

use shoal_core::process::usage::tree_memory;
use shoal_protocol::protocol::ScopeMemory;
use tokio::sync::watch;

/// How often the command's tree is read.
const INTERVAL: Duration = Duration::from_secs(1);

/// A running command's tree, read in the background.
pub(super) struct Watch {
    peak: Arc<Mutex<u64>>,
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
        Self { peak, task }
    }

    /// The most memory the command's tree held while it ran.
    pub(super) fn finish(self) -> ScopeMemory {
        self.task.abort();
        ScopeMemory {
            oom_kills: 0,
            peak: *self.peak.lock().expect("tree reads do not panic"),
        }
    }
}
