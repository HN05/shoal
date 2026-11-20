//! The space's status in the terminal the wrapper runs in, through the
//! Program Status Protocol: the wrapper writes each report the daemon sends
//! on Shoal's own record, only to a terminal that answered the protocol's
//! query before its child started, and reports the agent's exit itself, also
//! while the daemon is away.
mod query;
mod sequence;

use std::{
    fs::File,
    io::Write as _,
    os::unix::fs::OpenOptionsExt as _,
    sync::{Arc, Mutex, MutexGuard},
    time::Duration,
};

use shoal_protocol::program_status::{ProgramState, ProgramStatus};

/// How long the query waits for replies: only a terminal that answers
/// neither request keeps it waiting.
const QUERY_LIMIT: Duration = Duration::from_secs(3);

/// Shared by the link that receives the daemon's reports and the
/// supervisor that sees the agent exit.
#[derive(Clone, Default)]
pub(super) struct Reporter(Arc<Mutex<Reporting>>);

#[derive(Default)]
struct Reporting {
    /// The terminal, while it reads the protocol.
    terminal: Option<File>,
    /// The last report written, which the agent's exit follows.
    last: Option<ProgramStatus>,
}

impl Reporter {
    /// Ask the controlling terminal whether it reads the protocol, before
    /// a child starts there, and report to it from now on only if it does.
    pub(super) fn query(&self) -> bool {
        let terminal =
            controlling_terminal().filter(|terminal| query::reads_protocol(terminal, QUERY_LIMIT));
        let reads = terminal.is_some();
        self.lock().terminal = terminal;
        reads
    }

    /// Whether the terminal reads the protocol, as its last query found.
    pub(super) fn reports(&self) -> bool {
        self.lock().terminal.is_some()
    }

    /// Write `status`, the daemon's report of the space.
    pub(super) fn report(&self, status: ProgramStatus) {
        let mut reporting = self.lock();
        reporting.write(&status);
        reporting.last = Some(status);
    }

    /// Report that the agent exited with `code`, after a report of the
    /// space the daemon sent.
    pub(super) fn agent_exited(&self, code: i32) {
        let mut reporting = self.lock();
        if let Some(end) = (reporting.last.as_ref()).and_then(|last| exit_report(last, code)) {
            reporting.write(&end);
            reporting.last = Some(end);
        }
    }

    fn lock(&self) -> MutexGuard<'_, Reporting> {
        self.0
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }
}

impl Reporting {
    /// Write `status` to the terminal in one sequence; a failed write
    /// leaves the agent's terminal alone.
    fn write(&mut self, status: &ProgramStatus) {
        if let Some(terminal) = &mut self.terminal {
            let _ = terminal.write_all(&sequence::sequence(status));
        }
    }
}

/// What follows `last` once the agent exits with `code`: its failure, or
/// nothing in place of the turn its hooks reported, which ended with it.
/// What Shoal reported of its own records stays.
fn exit_report(last: &ProgramStatus, code: i32) -> Option<ProgramStatus> {
    let report = |state, message| ProgramStatus {
        state,
        title: last.title.clone(),
        message,
        turn: false,
    };
    if code != 0 {
        return Some(report(ProgramState::Error, Some(exit_message(code))));
    }
    last.turn.then(|| report(ProgramState::Clear, None))
}

/// The exit `code` as the wrapper reports it, which names a signal past 128.
fn exit_message(code: i32) -> String {
    match code - 128 {
        signal if signal > 0 => format!("The agent ended on signal {signal}"),
        _ => format!("The agent exited with code {code}"),
    }
}

/// The terminal this process runs in, for reading and writing.
fn controlling_terminal() -> Option<File> {
    std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .custom_flags(libc::O_NOCTTY)
        .open("/dev/tty")
        .ok()
}

#[cfg(test)]
mod tests;
