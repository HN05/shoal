//! Recent aggregate CPU use, sampled on a fixed cadence so dynamic agent
//! admission reads averages over a window instead of a single burst.
use std::{collections::VecDeque, sync::Arc, time::Duration};

use tokio::time::Instant;

use crate::{
    daemon::{
        log,
        workspace::{
            Manager,
            capacity::{self, CpuSample},
        },
    },
    process::load::CpuTicks,
};

const SAMPLE_INTERVAL: Duration = Duration::from_secs(5);
/// How late a reading may be before the sampler counts as stalled.
const STALE: Duration = Duration::from_secs(15);
/// The longest window admission averages over.
const RETAINED: Duration = capacity::SETTLED;

#[derive(Debug, Default)]
pub(crate) struct CpuUsage {
    samples: VecDeque<(Instant, CpuTicks)>,
}

impl CpuUsage {
    /// Add a reading. A failed or late one forgets the history, so no
    /// average spans a gap in what was measured.
    pub(crate) fn record(&mut self, now: Instant, ticks: anyhow::Result<CpuTicks>) {
        match ticks {
            Ok(ticks) => {
                if self.stale(now) {
                    self.samples.clear();
                }
                self.samples.push_back((now, ticks));
                while self
                    .samples
                    .get(1)
                    .is_some_and(|(time, _)| now.duration_since(*time) >= RETAINED)
                {
                    self.samples.pop_front();
                }
            }
            Err(error) => {
                // Once per gap: a platform without readings would repeat it forever.
                if !self.samples.is_empty() {
                    log!("CPU usage: {error:#}");
                }
                self.samples.clear();
            }
        }
    }

    /// The averages dynamic admission compares, once both are measured.
    pub(crate) fn admission(&self, now: Instant) -> Option<CpuSample> {
        Some(CpuSample {
            recent: self.average(now, capacity::RECENT)?,
            settled: self.average(now, capacity::SETTLED)?,
        })
    }

    /// Busy percentage across all cores over about the last `window`, or as
    /// much of it as was measured. `None` until two readings exist, or once
    /// sampling has stalled.
    fn average(&self, now: Instant, window: Duration) -> Option<f64> {
        if self.stale(now) {
            return None;
        }
        let &(newest_time, newest) = self.samples.back()?;
        let (_, oldest) = self
            .samples
            .iter()
            .find(|(time, _)| newest_time.duration_since(*time) <= window)?;
        newest.used_percent_since(*oldest)
    }

    fn stale(&self, now: Instant) -> bool {
        self.samples
            .back()
            .is_some_and(|(time, _)| now.duration_since(*time) > STALE)
    }
}

pub(super) async fn run(manager: Arc<Manager>) {
    loop {
        let ticks = crate::process::load::cpu_ticks();
        manager
            .usage
            .with_cpu(|cpu| cpu.record(Instant::now(), ticks));
        tokio::time::sleep(SAMPLE_INTERVAL).await;
    }
}

#[cfg(test)]
mod tests;
