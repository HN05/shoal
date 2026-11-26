//! Watches of linked items: the polls that read them, the cursors that
//! record what each space saw, and what they deliver.
pub mod activity;
pub mod background;
pub mod branch_report;
pub mod changes;
pub mod cursor;
pub mod delivery;
pub mod elsewhere;
pub mod followed;
pub mod framing;
pub mod merge_lookups;
mod merge_runs;
pub mod poll;
pub mod reads;
pub mod selection;
pub mod start;
pub mod starts;
mod waits;
pub mod wake_log;

pub use waits::{ActiveWait, ActiveWaits};

use shoal_protocol::{alerts::Alert, links::Updates};

/// The responses a watch gives without updates: one a newer wait
/// superseded, one that timed out, and one with only messages.
pub trait UpdatesExt {
    fn superseded() -> Self;

    fn timed_out() -> Self;

    fn messages(messages: Vec<Alert>) -> Self;
}

impl UpdatesExt for Updates {
    fn superseded() -> Self {
        Self {
            updates: Vec::new(),
            alerts: Vec::new(),
            leftovers: Vec::new(),
            built_directly: None,
            timed_out: false,
            superseded: true,
        }
    }

    fn timed_out() -> Self {
        Self {
            updates: Vec::new(),
            alerts: Vec::new(),
            leftovers: Vec::new(),
            built_directly: None,
            timed_out: true,
            superseded: false,
        }
    }

    fn messages(messages: Vec<Alert>) -> Self {
        Self {
            updates: Vec::new(),
            alerts: messages,
            leftovers: Vec::new(),
            built_directly: None,
            timed_out: false,
            superseded: false,
        }
    }
}
