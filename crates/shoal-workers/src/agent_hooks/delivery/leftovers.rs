//! How an agent hook tells the agent about the processes its commands left
//! running.
use std::time::Duration;

use shoal_protocol::{processes::Leftover, time::UnixTime};

/// The most bytes of a leftover's command a notice shows.
const COMMAND_LIMIT: usize = 80;

/// The notice about `leftovers` of the `space`, "workspace" or "chat", at
/// `now`.
pub(super) fn notice(space: &str, leftovers: &[Leftover], now: UnixTime) -> String {
    let listed = leftovers
        .iter()
        .map(|leftover| {
            format!(
                "{} (PID {}, {})",
                shoal_text::cut_bytes(leftover.command.clone(), COMMAND_LIMIT),
                leftover.pid,
                running_time(now.duration_since(leftover.started_at))
            )
        })
        .collect::<Vec<_>>()
        .join(", ");
    match leftovers {
        [_] => format!(
            "1 process from this {space} is still running after its parent exited: {listed}. Stop it with shoal stop --leftovers."
        ),
        _ => format!(
            "{} processes from this {space} are still running after their parent exited: {listed}. Stop them with shoal stop --leftovers.",
            leftovers.len()
        ),
    }
}

/// How long a process has run, from `running`.
fn running_time(running: Duration) -> String {
    let minutes = running.as_secs() / 60;
    match minutes {
        0 => "under 1 min".to_owned(),
        1..120 => format!("{minutes} min"),
        _ if minutes < 48 * 60 => format!("{} h", minutes / 60),
        _ => format!("{} days", minutes / (24 * 60)),
    }
}

#[cfg(test)]
mod tests;
