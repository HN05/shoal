//! What watches keep and answer: the running wait of each space, each
//! item's record on a space's watch cursor and what it holds back, the
//! items it follows, its merge lookups and the runs they find, what a
//! branch's local merge check reports, the polls, what a watch or hook
//! delivers or the hub carries to a coordinator elsewhere, and how a
//! response fits one frame.
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
            timed_out: false,
            superseded: true,
        }
    }

    fn timed_out() -> Self {
        Self {
            updates: Vec::new(),
            alerts: Vec::new(),
            leftovers: Vec::new(),
            timed_out: true,
            superseded: false,
        }
    }

    fn messages(messages: Vec<Alert>) -> Self {
        Self {
            updates: Vec::new(),
            alerts: messages,
            leftovers: Vec::new(),
            timed_out: false,
            superseded: false,
        }
    }
}
