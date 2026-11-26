//! Log the memory readings protection decides on while memory is past a
//! warning or stop level, so the daemon log traces how pressure built up.
use std::{fmt::Write, time::Duration};
use tokio::time::Instant;

use crate::log;
use crate::process::load::{MemoryPressure, MemoryReading, Stall, WorkMemory};

/// How long a reading at a level already logged waits before it is logged
/// again; a higher level is logged at once.
const REPEAT: Duration = Duration::from_secs(30);

/// How far memory is past its levels.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum Level {
    Warning,
    Stop,
}

impl Level {
    fn of(pressure: MemoryPressure) -> Option<Self> {
        if pressure.critical || pressure.stalled {
            Some(Self::Stop)
        } else if pressure.warning {
            Some(Self::Warning)
        } else {
            None
        }
    }

    fn name(self) -> &'static str {
        match self {
            Self::Warning => "warning",
            Self::Stop => "stop",
        }
    }
}

/// The last reading logged, and its level, while memory stays pressed.
#[derive(Default)]
pub(super) struct PressureLog {
    last: Option<(Instant, Level)>,
}

impl PressureLog {
    /// Log `reading`, taken at `now`, where it is due.
    pub(super) fn observe(&mut self, now: Instant, reading: &MemoryReading) {
        if let Some(level) = self.due(now, reading.pressure) {
            log!(
                "memory past its {} level: {}",
                level.name(),
                describe(reading)
            );
        }
    }

    /// The level of a reading at `now` under `pressure` that is due to be
    /// logged; a calm one ends the stretch, so the next pressure logs at once.
    fn due(&mut self, now: Instant, pressure: MemoryPressure) -> Option<Level> {
        let Some(level) = Level::of(pressure) else {
            self.last = None;
            return None;
        };
        if let Some((at, logged)) = self.last
            && level <= logged
            && now.duration_since(at) < REPEAT
        {
            return None;
        }
        self.last = Some((now, level));
        Some(level)
    }
}

/// The figures of `reading` that its pressure stands on.
fn describe(reading: &MemoryReading) -> String {
    let mut text = format!(
        "{:.1}% in use, {}",
        reading.memory.used_percent(),
        stall(reading.stall)
    );
    match reading.work {
        Some(WorkMemory::Read {
            memory,
            stall: work,
        }) => {
            let _ = write!(
                text,
                "; Shoal's work {:.1}% of its limit, {}",
                memory.used_percent(),
                stall(work)
            );
        }
        Some(WorkMemory::Unread) => text.push_str("; Shoal's work unread"),
        None => {}
    }
    text
}

fn stall(stall: Option<Stall>) -> String {
    match stall {
        Some(stall) => format!(
            "stall some {:.2}% full {:.2}% over ten seconds",
            stall.some_avg10, stall.full_avg10
        ),
        None => "no stall reading".to_owned(),
    }
}

#[cfg(test)]
mod tests;
