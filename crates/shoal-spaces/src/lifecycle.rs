//! The one removal path every space takes, explicit or by cleanup. See
//! `docs/design/cleanup.md`.
pub mod abandoned;
pub mod background;
mod candidacy;
mod conversion;
mod policy;
mod remove;
#[cfg(any(test, feature = "test-support"))]
mod test_support;
pub mod worktree;

pub use abandoned::STOPPED_REMOVAL;
pub use abandoned::Underway;
pub use abandoned::fail_interrupted_removals;

pub use policy::Removal;
use policy::{Retention, Stage};
pub use worktree::{WorktreeRemoval, not_ready};

#[cfg(test)]
mod tests;
