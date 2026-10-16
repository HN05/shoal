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
            .cpu_usage
            .lock()
            .unwrap()
            .record(Instant::now(), ticks);
        tokio::time::sleep(SAMPLE_INTERVAL).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ticks(busy: u64, total: u64) -> anyhow::Result<CpuTicks> {
        Ok(CpuTicks { busy, total })
    }

    #[tokio::test(start_paused = true)]
    async fn averages_cover_the_window_and_forget_gaps() {
        let mut usage = CpuUsage::default();
        let start = Instant::now();
        assert_eq!(usage.average(start, RETAINED), None);
        usage.record(start, ticks(0, 0));
        assert_eq!(usage.average(start, RETAINED), None);
        usage.record(start + SAMPLE_INTERVAL, ticks(100, 100));
        usage.record(start + SAMPLE_INTERVAL * 2, ticks(100, 200));
        let now = start + SAMPLE_INTERVAL * 2;
        // The window reaches back to the oldest reading inside it.
        assert_eq!(usage.average(now, SAMPLE_INTERVAL), Some(0.0));
        assert_eq!(usage.average(now, RETAINED), Some(50.0));
        // A stalled sampler authorizes nothing, and its next reading starts over.
        let late = now + STALE + SAMPLE_INTERVAL;
        assert_eq!(usage.average(late, RETAINED), None);
        usage.record(late, ticks(300, 400));
        assert_eq!(usage.average(late, RETAINED), None);
        usage.record(late, Err(anyhow::anyhow!("unavailable")));
        assert!(usage.samples.is_empty());
    }

    #[tokio::test(start_paused = true)]
    async fn readings_older_than_the_longest_window_are_dropped() {
        let mut usage = CpuUsage::default();
        let start = Instant::now();
        let intervals = (RETAINED.as_secs() / SAMPLE_INTERVAL.as_secs()) as u32;
        for interval in 0..=intervals + 2 {
            usage.record(
                start + SAMPLE_INTERVAL * interval,
                ticks(0, interval.into()),
            );
        }
        assert_eq!(usage.samples.len(), intervals as usize + 1);
    }
}
