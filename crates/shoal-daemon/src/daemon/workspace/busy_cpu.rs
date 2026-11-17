//! The machine's busy CPU, niced time included, over the overload monitor's
//! recent samples, which dynamic job admission reads: CI jobs run niced, so
//! only this reading shows them competing for the cores.
use std::{collections::VecDeque, sync::Mutex, time::Duration};

use tokio::time::Instant;

use crate::process::load::CpuTicks;

#[derive(Default)]
pub(crate) struct BusyCpu {
    /// Continuous samples, oldest first, back to the newest one a window
    /// old.
    samples: Mutex<VecDeque<(Instant, CpuTicks)>>,
}

impl BusyCpu {
    /// Keep the `ticks` sampled at `now`, or forget every sample on a
    /// failed reading or one more than `gap` after the previous, since a
    /// window must be continuous. Samples older than the newest `window` old
    /// go.
    pub(crate) fn observe(
        &self,
        now: Instant,
        ticks: Option<CpuTicks>,
        gap: Duration,
        window: Duration,
    ) {
        let mut samples = self.samples.lock().unwrap();
        let Some(ticks) = ticks else {
            samples.clear();
            return;
        };
        if samples
            .back()
            .is_some_and(|(previous, _)| now.duration_since(*previous) > gap)
        {
            samples.clear();
        }
        samples.push_back((now, ticks));
        while samples
            .get(1)
            .is_some_and(|(time, _)| now.duration_since(*time) >= window)
        {
            samples.pop_front();
        }
    }

    /// The busy percentage between the newest sample and the newest one at
    /// least `window` older; `None` until the samples span the window.
    pub(crate) fn percent(&self, window: Duration) -> Option<f64> {
        let samples = self.samples.lock().unwrap();
        let (newest, ticks) = samples.back()?;
        let (_, earlier) = samples
            .iter()
            .rev()
            .find(|(time, _)| newest.duration_since(*time) >= window)?;
        ticks.busy_percent_since(*earlier)
    }
}

#[cfg(test)]
mod tests;
