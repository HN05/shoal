//! The steps of cleanup and removal that only a space with a worktree
//! takes: [`candidacy`] inspects its commands, leases, Git state, files and
//! the processes working in it, and [`removal`] stops what runs there, runs
//! its removal hooks and deletes it once a checkpoint keeps its work.
mod candidacy;
mod removal;

pub(crate) use candidacy::not_ready;
pub(crate) use removal::WorktreeRemoval;
pub(in crate::daemon::workspace) use removal::WorktreeStage;
#[cfg(test)]
pub(in crate::daemon::workspace) use removal::removal_options;
