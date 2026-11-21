//! Delivering the activity of a space's linked items, with its messages, to
//! its agent through the space's watch cursors, and the response type a
//! watch or hook returns; waiting does not change completion policy.

pub(super) use self::activity::record_received_change;
pub(crate) use self::start::WatchStarts;
use shoal_links::wait::{branch_report, cursor};

pub(crate) use shoal_protocol::links::Updates;

mod activity;
mod background;
mod delivery;
mod elsewhere;
mod items;
mod merge_runs;
mod poll;
mod start;
mod wake;

#[cfg(test)]
pub(crate) mod test_support;
#[cfg(test)]
mod tests;
