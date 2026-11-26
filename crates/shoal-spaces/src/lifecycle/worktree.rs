//! The steps of cleanup and removal that only a space with a worktree
//! takes.
mod candidacy;
mod removal;

pub use candidacy::not_ready;
pub use removal::WorktreeRemoval;
pub use removal::WorktreeStage;
#[cfg(any(test, feature = "test-support"))]
pub use removal::removal_options;
