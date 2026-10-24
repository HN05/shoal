//! Timing lines for profiling Shoal itself. With `SHOAL_PROFILE=1`, a process
//! prints to stderr how long each of its daemon requests and subprocess runs
//! took; the daemon's stderr is its log.
use std::{
    io::Write,
    sync::OnceLock,
    time::{Duration, Instant},
};

pub fn enabled() -> bool {
    static ENABLED: OnceLock<bool> = OnceLock::new();
    *ENABLED.get_or_init(|| std::env::var_os(crate::env::PROFILE).is_some_and(|value| value == "1"))
}

/// Time every subprocess run from now on, when profiling.
pub fn start() {
    if enabled() {
        crate::subprocess::observe(|command, elapsed| line("run", elapsed, command));
    }
}

/// Prints a `kind` timing line for `subject` when dropped.
pub struct Span {
    kind: &'static str,
    subject: String,
    started: Instant,
}

/// A span for `subject`, or none when not profiling.
pub fn span(kind: &'static str, subject: impl FnOnce() -> String) -> Option<Span> {
    enabled().then(|| Span {
        kind,
        subject: subject(),
        started: Instant::now(),
    })
}

impl Drop for Span {
    fn drop(&mut self) {
        line(self.kind, self.started.elapsed(), &self.subject);
    }
}

fn line(kind: &str, elapsed: Duration, subject: &str) {
    let millis = elapsed.as_secs_f64() * 1000.0;
    // One write per line keeps concurrent tasks from interleaving within it.
    let text = format!("profile {kind} {millis:.1}ms {subject}\n");
    let _ = std::io::stderr().lock().write_all(text.as_bytes());
}
