//! Transient CLI feedback while an operation is pending.
use std::{
    future::Future,
    io::{self, IsTerminal, Write},
    time::Duration,
};

use tokio::time::{Instant, Interval, MissedTickBehavior, interval_at};

/// Only wrap silent work: this owns stderr until the future finishes or drops.
pub async fn run<T>(json: bool, message: &str, work: impl Future<Output = T>) -> T {
    spin(Spinner::stderr(json, message), work).await
}

async fn spin<T>(mut spinner: Spinner<impl Write>, work: impl Future<Output = T>) -> T {
    tokio::pin!(work);
    loop {
        tokio::select! {
            biased;
            result = &mut work => return result,
            () = spinner.tick() => {}
        }
    }
}

/// A line on stderr showing what is pending and for how long, from half a
/// second on, which a caller erases before writing a line of its own; the
/// next frame draws it again below. Hidden with JSON output and where stderr
/// is no terminal that redraws.
pub struct Spinner<W: Write> {
    started: Instant,
    /// `None` while hidden.
    ticks: Option<Interval>,
    frame: usize,
    message: String,
    line: Line<W>,
}

impl Spinner<io::Stderr> {
    pub fn stderr(json: bool, message: &str) -> Self {
        let stderr = io::stderr();
        let term = std::env::var_os("TERM");
        let shown = shows(json, stderr.is_terminal(), term.as_deref());
        Self::new(shown, message, stderr)
    }
}

impl<W: Write> Spinner<W> {
    fn new(shown: bool, message: &str, output: W) -> Self {
        let started = Instant::now();
        let ticks = shown.then(|| {
            let mut ticks = interval_at(
                started + Duration::from_millis(500),
                Duration::from_millis(100),
            );
            ticks.set_missed_tick_behavior(MissedTickBehavior::Skip);
            ticks
        });
        Self {
            started,
            ticks,
            frame: 0,
            message: message.to_owned(),
            line: Line { width: 0, output },
        }
    }

    /// Show `message` from the next frame on, keeping the elapsed time.
    pub fn set_message(&mut self, message: &str) {
        message.clone_into(&mut self.message);
    }

    /// Draw the next frame once it is due; never completes while hidden.
    pub async fn tick(&mut self) {
        match &mut self.ticks {
            Some(ticks) => ticks.tick().await,
            None => std::future::pending().await,
        };
        let spinner = ['|', '/', '-', '\\'][self.frame % 4];
        let elapsed = self.started.elapsed().as_secs();
        (self.line).draw(&format!("{spinner} {} ({elapsed}s)", self.message));
        self.frame += 1;
    }

    /// Erase the drawn frame, so a line written next starts at the margin.
    pub fn clear(&mut self) {
        self.line.clear();
    }
}

fn shows(json: bool, terminal: bool, term: Option<&std::ffi::OsStr>) -> bool {
    !json && terminal && term != Some(std::ffi::OsStr::new("dumb"))
}

struct Line<W: Write> {
    width: usize,
    output: W,
}

impl<W: Write> Line<W> {
    fn draw(&mut self, text: &str) {
        self.width = self.width.max(text.len());
        let _ = write!(self.output, "\r{text:width$}", width = self.width);
        let _ = self.output.flush();
    }

    fn clear(&mut self) {
        if self.width > 0 {
            let _ = write!(self.output, "\r{:width$}\r", "", width = self.width);
            let _ = self.output.flush();
            self.width = 0;
        }
    }
}

impl<W: Write> Drop for Line<W> {
    fn drop(&mut self) {
        self.clear();
    }
}

#[cfg(test)]
mod tests;
