//! What a start or a creation reads of a workspace's preparation: the
//! workspace a launch recorded, followed to its end, reported or entered.
mod finish;
mod launch;
mod prepared;

pub(in crate::cli::commands) use finish::enter_workspace;
pub(in crate::cli::commands) use launch::{enter_created, report_launched};
pub(in crate::cli::commands) use prepared::{follow_preparation, shell_in, unprepared};
