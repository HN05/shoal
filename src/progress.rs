//! Transient CLI feedback while a silent operation is pending.
use std::{
    future::Future,
    io::{self, IsTerminal, Write},
    time::Duration,
};

use tokio::time::{Instant, MissedTickBehavior, interval_at};

/// Only wrap silent work: this owns stderr until the future finishes or drops.
pub async fn run<T>(json: bool, message: &str, work: impl Future<Output = T>) -> T {
    if json || !io::stderr().is_terminal() || std::env::var_os("TERM").is_some_and(|t| t == "dumb")
    {
        return work.await;
    }
    let started = Instant::now();
    let mut ticks = interval_at(
        started + Duration::from_millis(500),
        Duration::from_millis(100),
    );
    ticks.set_missed_tick_behavior(MissedTickBehavior::Skip);
    let mut line = Line { width: 0 };
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

struct Line {
    width: usize,
}

impl Line {
    fn draw(&mut self, text: &str) {
        self.width = self.width.max(text.len());
        let mut stderr = io::stderr().lock();
        let _ = write!(stderr, "\r{text:width$}", width = self.width);
        let _ = stderr.flush();
    }
}

impl Drop for Line {
    fn drop(&mut self) {
        if self.width > 0 {
            let mut stderr = io::stderr().lock();
            let _ = write!(stderr, "\r{:width$}\r", "", width = self.width);
            let _ = stderr.flush();
        }
    }
}
