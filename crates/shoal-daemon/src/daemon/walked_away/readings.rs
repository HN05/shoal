//! What macOS's `ioreg` and `pmset` print of the lid, the user's last
//! input and the idle sleep timer.
use std::time::Duration;

/// Whether the lid is closed, from `ioreg -r -k AppleClamshellState -d 1`;
/// none on a Mac without a lid, which lists no `AppleClamshellState`.
pub(super) fn clamshell_closed(ioreg: &str) -> Option<bool> {
    match property(ioreg, "AppleClamshellState")? {
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
