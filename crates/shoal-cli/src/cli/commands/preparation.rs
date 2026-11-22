//! What a start, a creation or a setup reads of a workspace's preparation:
//! the workspace a launch recorded, followed to its end, reported or
//! entered, and its shell once prepared.
mod prepared;
mod reports;
mod starting;

pub(super) use prepared::workspace_start;
pub(super) use reports::{enter_created, location_phrase, prepared_on, report_launched};
