//! Delivering the activity of a space's linked items, with its messages, to
//! its agent through the space's watch cursors, and the response type a
//! watch or hook returns; waiting does not change completion policy.

pub(super) use self::activity::{queue_update, record_received_change};
pub(crate) use self::{
    activity::{OwnChange, record_own_change},
    framing::RESPONSE_LIMIT,
    items::{assigned_items, linked_items, open_prs, watches_open_pr},
    waits::ActiveWaits,
};
use crate::daemon::agent_messages::AgentMessage;

pub(crate) use crate::client::links::Updates;

mod activity;
mod background;
mod branch_report;
mod cursor;
mod delivery;
mod framing;
mod items;
mod poll;
mod waits;
mod wake;
mod wake_log;

/// Operations on the client type [`Updates`] that only the `shoal` command uses.
pub(crate) trait UpdatesExt {
    fn superseded() -> Self;

    fn timed_out() -> Self;

    fn messages(messages: Vec<AgentMessage>) -> Self;
}

impl UpdatesExt for Updates {
    fn superseded() -> Self {
        Self {
            updates: Vec::new(),
            messages: Vec::new(),
            leftovers: Vec::new(),
            timed_out: false,
            superseded: true,
        }
    }

    fn timed_out() -> Self {
        Self {
            updates: Vec::new(),
            messages: Vec::new(),
            leftovers: Vec::new(),
            timed_out: true,
            superseded: false,
        }
    }

    fn messages(messages: Vec<AgentMessage>) -> Self {
        Self {
            updates: Vec::new(),
            messages,
            leftovers: Vec::new(),
            timed_out: false,
            superseded: false,
        }
    }
}

#[cfg(test)]
mod tests;
