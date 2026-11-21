//! What a start, a creation or a setup reads of a workspace's preparation:
//! the workspace a launch recorded, followed to its end, reported or
//! entered, and its shell once prepared.
mod finish;
mod prepared;
mod reports;

pub(super) use finish::enter_workspace;
pub(super) use prepared::workspace_start;
pub(super) use prepared::{follow_preparation, shell_in, unprepared};
pub(super) use reports::{enter_created, location_phrase, prepared_on, report_launched};
