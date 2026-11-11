//! One removal path for every space, explicit or by cleanup: [`policy`]
//! decides what each removal checks, [`candidacy`] what keeps a space from
//! cleanup, [`remove`] carries removal out, which [`background`] lets a
//! request leave running, and [`worktree`] adds the steps only a space with
//! a worktree takes; [`conversion`] removes the worktree of a workspace that
//! becomes a chat along the same path.
mod background;
mod candidacy;
mod conversion;
mod policy;
mod remove;
#[cfg(test)]
mod test_support;
#[cfg(test)]
mod tests;
mod worktree;

pub(in crate::daemon::workspace) use policy::Removal;
use policy::{Retention, Stage};
pub(crate) use worktree::{WorktreeRemoval, not_ready};
