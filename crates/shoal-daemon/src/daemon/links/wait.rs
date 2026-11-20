//! Delivering the activity of a space's linked items, with its messages, to
//! its agent through the space's watch cursors, and the response type a
//! watch or hook returns; waiting does not change completion policy.

pub(super) use self::activity::{queue_update, record_received_change, record_shoal_change};
pub(crate) use self::{
    activity::{OwnChange, record_own_change},
    items::{followed_items, linked_items, watches_open_pr},
    merge_runs::PushedMerge,
    start::WatchStarts,
};
use crate::daemon::alerts::Alert;
use shoal_links::wait::{branch_report, cursor};

pub(crate) use shoal_protocol::links::Updates;

mod activity;
mod background;
mod delivery;
mod elsewhere;
mod framing;
mod items;
mod merge_runs;
mod poll;
mod start;
mod wake;
mod wake_log;

/// Operations on the client type [`Updates`] that only the `shoal` command uses.
pub(crate) trait UpdatesExt {
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

#[cfg(test)]
pub(crate) mod test_support;
#[cfg(test)]
mod tests;
