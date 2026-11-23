//! One removal path for every space, explicit or by cleanup: [`policy`]
//! decides what each removal checks, [`candidacy`] what keeps a space from
//! cleanup, [`remove`] carries removal out, which [`background`] lets a
//! request leave running and [`abandoned`] fails when it stops before it
//! finishes, and [`worktree`] adds the steps only a space with a worktree
//! takes; [`conversion`] removes the worktree of a workspace that becomes a
//! chat along the same path.
pub mod abandoned;
pub mod background;
mod candidacy;
mod conversion;
mod policy;
mod remove;
#[cfg(any(test, feature = "test-support"))]
// The overload tests in `shoal-workers` read some of it.
#[cfg_attr(not(test), allow(dead_code))]
mod test_support;
pub mod worktree;

pub use abandoned::STOPPED_REMOVAL;
pub use abandoned::Underway;
pub use abandoned::fail_interrupted_removals;

pub use policy::Removal;
use policy::{Retention, Stage};
pub use worktree::{WorktreeRemoval, not_ready};
