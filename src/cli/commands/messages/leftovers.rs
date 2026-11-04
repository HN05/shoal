//! How an agent hook tells the agent about the leftovers of its workspace or
//! chat, processes its commands left running, and how to stop them.
use crate::client::processes::Leftover;

/// The longest command a notice shows of a leftover.
const COMMAND_LIMIT: usize = 80;

/// The notice about `leftovers` of the `space`, "workspace" or "chat", at
/// `now` in Unix seconds.
pub(super) fn notice(space: &str, leftovers: &[Leftover], now: u64) -> String {
    let listed = leftovers
        .iter()
        .map(|leftover| {
            format!(
                "{} (PID {}, {})",
                shortened(&leftover.command),
                leftover.pid,
                running_time(now.saturating_sub(leftover.started_at))
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

/// `command` cut to [`COMMAND_LIMIT`] characters.
fn shortened(command: &str) -> String {
    match command.char_indices().nth(COMMAND_LIMIT) {
        Some((end, _)) => format!("{} …", command[..end].trim_end()),
        None => command.to_owned(),
    }
}

/// How long a process has run, from `seconds`.
fn running_time(seconds: u64) -> String {
    let minutes = seconds / 60;
    match minutes {
        0 => "under 1 min".to_owned(),
        1..120 => format!("{minutes} min"),
        _ if minutes < 48 * 60 => format!("{} h", minutes / 60),
        _ => format!("{} days", minutes / (24 * 60)),
    }
}

#[cfg(test)]
mod tests;
