//! What a start and the queue's lists say of an agent waiting in the agent
//! queue.
use shoal_protocol::{model::QueueEntry, state::QueueState};

/// When the agent of `entry` starts, as a list or a start says it.
pub fn starts_when(entry: &QueueEntry) -> String {
    match entry.blockers.as_slice() {
        [] => "when a heavy slot is free".to_owned(),
        blockers => format!(
            "once it is no longer blocked by {} and a heavy slot is free",
            blockers.join(", ")
        ),
    }
}

/// What a start says of the agent of `entry`, queued in the space `name`.
pub fn queued_in(name: &str, entry: &QueueEntry) -> String {
    match entry.state {
        QueueState::Queued => format!(
            "Queued {} in {name} (position {}); it starts {}",
            entry.agent,
            entry.position.unwrap_or(1),
            starts_when(entry)
        ),
        _ => format!("Starting {} in {name}", entry.agent),
    }
}
