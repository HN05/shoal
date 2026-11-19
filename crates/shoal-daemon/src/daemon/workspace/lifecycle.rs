//! One removal path for every space, explicit or by cleanup: [`policy`]
//! decides what each removal checks, [`candidacy`] what keeps a space from
//! cleanup, [`remove`] carries removal out, which [`background`] lets a
//! request leave running and [`abandoned`] fails when it stops before it
//! finishes, and [`worktree`] adds the steps only a space with a worktree
//! takes; [`conversion`] removes the worktree of a workspace that becomes a
//! chat along the same path.
mod abandoned;
mod background;
mod candidacy;
mod conversion;
mod policy;
mod remove;
#[cfg(any(test, feature = "test-support"))]
// The overload tests in `shoal-workers` read some of it.
#[cfg_attr(not(test), allow(dead_code))]
mod test_support;
#[cfg(test)]
mod tests;
mod worktree;

pub(in crate::daemon) use abandoned::STOPPED_REMOVAL;
pub(in crate::daemon::workspace) use abandoned::Underway;
pub(crate) use abandoned::fail_interrupted_removals;
pub(crate) use policy::BelowElsewhere;
pub(in crate::daemon) use policy::Removal;
use policy::{Retention, Stage};
pub(crate) use worktree::{WorktreeRemoval, not_ready};
