//! The steps of cleanup and removal that only a space with a worktree
//! takes: [`candidacy`] inspects its commands, leases, Git state, files and
//! the processes working in it, and [`removal`] stops what runs there, runs
//! its removal hooks and deletes it once a checkpoint keeps its work.
mod candidacy;
mod removal;

pub use candidacy::not_ready;
pub use removal::WorktreeRemoval;
pub use removal::WorktreeStage;
#[cfg(any(test, feature = "test-support"))]
pub use removal::removal_options;
