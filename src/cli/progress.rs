//! Transient CLI feedback while a silent operation is pending.
use std::{
    future::Future,
    io::{self, IsTerminal, Write},
    time::Duration,
};

use tokio::time::{Instant, MissedTickBehavior, interval_at};

/// Only wrap silent work: this owns stderr until the future finishes or drops.
pub async fn run<T>(json: bool, message: &str, work: impl Future<Output = T>) -> T {
    let term = std::env::var_os("TERM");
    let mut stderr = io::stderr();
    run_on(
        json,
        stderr.is_terminal(),
        term.as_deref(),
        message,
        work,
        &mut stderr,
    )
    .await
}

async fn run_on<T>(
    json: bool,
    terminal: bool,
    term: Option<&std::ffi::OsStr>,
    message: &str,
    work: impl Future<Output = T>,
    output: &mut impl Write,
) -> T {
    if json || !terminal || term == Some(std::ffi::OsStr::new("dumb")) {
        return work.await;
    }
    render(message, work, output).await
}

async fn render<T>(message: &str, work: impl Future<Output = T>, output: &mut impl Write) -> T {
    let started = Instant::now();
    let mut ticks = interval_at(
        started + Duration::from_millis(500),
        Duration::from_millis(100),
    );
    ticks.set_missed_tick_behavior(MissedTickBehavior::Skip);
    let mut line = Line { width: 0, output };
    let mut frame = 0;
    tokio::pin!(work);
    loop {
        tokio::select! {
            biased;
            result = &mut work => return result,
            _ = ticks.tick() => {
                let spinner = ['|', '/', '-', '\\'][frame % 4];
                line.draw(&format!("{spinner} {message} ({}s)", started.elapsed().as_secs()));
                frame += 1;
            }
        }
    }
}

struct Line<'a, W: Write> {
    width: usize,
    output: &'a mut W,
}

impl<W: Write> Line<'_, W> {
    fn draw(&mut self, text: &str) {
        self.width = self.width.max(text.len());
        let _ = write!(self.output, "\r{text:width$}", width = self.width);
        let _ = self.output.flush();
    }
}

impl<W: Write> Drop for Line<'_, W> {
    fn drop(&mut self) {
        if self.width > 0 {
            let _ = write!(self.output, "\r{:width$}\r", "", width = self.width);
            let _ = self.output.flush();
        }
    }
}

#[cfg(test)]
mod tests;
