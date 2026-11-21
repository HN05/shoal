//! Delivering the activity of a space's linked items, with its messages, to
//! its agent through the space's watch cursors, and the response type a
//! watch or hook returns; waiting does not change completion policy.

use shoal_links::wait::branch_report;

pub(crate) use shoal_protocol::links::Updates;

mod delivery;
mod elsewhere;
mod merge_runs;
mod poll;
mod start;
mod wake;

#[cfg(test)]
pub(crate) mod test_support;
#[cfg(test)]
mod tests;
