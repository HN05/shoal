//! The task that writes reports to the terminal, apart from the wrapper's
//! control path, so a terminal that stops draining holds back only the
//! reports.
use std::{fs::File, io::Write as _, time::Duration};

use tokio::sync::watch;

/// How long the agent's exit report waits for the terminal before the
/// wrapper goes on.
const EXIT_LIMIT: Duration = Duration::from_secs(2);

pub(super) struct Writer {
    /// The latest report's number and sequence.
    latest: watch::Sender<(u64, Vec<u8>)>,
    /// The number of the last report written.
    written: watch::Receiver<u64>,
}

impl Writer {
    /// Write the reports queued from now on to `terminal`.
    pub(super) fn start(terminal: File) -> Self {
        let (latest, queued) = watch::channel((0, Vec::new()));
        let (done, written) = watch::channel(0);
        tokio::spawn(write_reports(terminal, queued, done));
        Self { latest, written }
    }

    /// Queue `sequence` in place of any report not yet written.
    pub(super) fn queue(&self, sequence: Vec<u8>) -> Written {
        let mut number = 0;
        self.latest.send_modify(|(latest, queued)| {
            *latest += 1;
            number = *latest;
            *queued = sequence;
        });
        Written {
            written: self.written.clone(),
            number,
        }
    }
}

/// A queued report, which can be waited for.
pub(super) struct Written {
    written: watch::Receiver<u64>,
    number: u64,
}

impl Written {
    /// Wait until the report, or one queued after it, is written, for at
    /// most the exit's limit.
    pub(super) async fn wait(mut self) {
        let number = self.number;
        let written = self.written.wait_for(|written| *written >= number);
        let _ = tokio::time::timeout(EXIT_LIMIT, written).await;
    }
}

/// Write each latest report in one write, off the runtime's workers; a
/// failed write leaves the agent's terminal alone.
async fn write_reports(
    mut terminal: File,
    mut queued: watch::Receiver<(u64, Vec<u8>)>,
    done: watch::Sender<u64>,
) {
    while queued.changed().await.is_ok() {
        let (number, sequence) = queued.borrow_and_update().clone();
        let writing = tokio::task::spawn_blocking(move || {
            let _ = terminal.write_all(&sequence);
            terminal
        });
        let Ok(returned) = writing.await else {
            return;
        };
        terminal = returned;
        done.send_replace(number);
    }
}
