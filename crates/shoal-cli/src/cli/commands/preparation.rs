//! Following and reporting a workspace's preparation for a start, a creation or
//! a setup.
mod prepared;
mod reports;
mod starting;

pub(super) use prepared::workspace_start;
pub(super) use reports::{enter_created, location_phrase, prepared_on, report_launched};
