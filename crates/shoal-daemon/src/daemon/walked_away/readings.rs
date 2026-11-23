//! What macOS's `ioreg` and `pmset` print of the lid, the user's last
//! input and the idle sleep timer.
use std::time::Duration;

use super::Lid;

/// The lid, from `ioreg -r -k AppleClamshellState -d 1`; none on a Mac
/// without one, which lists no `AppleClamshellState`. A closed lid that
/// does not sleep the Mac, as with an external display, is docked.
pub(super) fn lid(ioreg: &str) -> Option<Lid> {
    let closed = yes(property(ioreg, "AppleClamshellState")?)?;
    let sleeps = property(ioreg, "AppleClamshellCausesSleep").and_then(yes);
    Some(Lid {
        closed,
        docked: sleeps == Some(false),
    })
}

/// An IOKit boolean as `ioreg` prints it.
fn yes(value: &str) -> Option<bool> {
    match value {
        "Yes" => Some(true),
        "No" => Some(false),
        _ => None,
    }
}

/// How long ago the user's last input came, from `ioreg -r -c IOHIDSystem
/// -d 1`.
pub(super) fn idle_time(ioreg: &str) -> Option<Duration> {
    let nanoseconds = property(ioreg, "HIDIdleTime")?.parse().ok()?;
    Some(Duration::from_nanos(nanoseconds))
}

/// The idle time after which the system sleeps, from `pmset -g`; none when
/// it never sleeps for idleness.
pub(super) fn sleep_timer(pmset: &str) -> Option<Duration> {
    let minutes: u64 = pmset.lines().find_map(|line| {
        let mut words = line.split_whitespace();
        (words.next()? == "sleep").then_some(())?;
        words.next()?.parse().ok()
    })?;
    (minutes > 0).then(|| Duration::from_secs(minutes * 60))
}

/// The value of the property `name`, printed as `"name" = value`.
fn property<'a>(ioreg: &'a str, name: &str) -> Option<&'a str> {
    let key = format!("\"{name}\" = ");
    (ioreg.lines()).find_map(|line| Some(line.split_once(&key)?.1.trim()))
}

#[cfg(test)]
mod tests;
