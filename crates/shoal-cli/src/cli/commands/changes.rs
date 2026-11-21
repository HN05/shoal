//! `shoal changes`: the hunks Shoal records as the agents it runs change
//! the files of their worktrees.
mod diffs;
mod listing;

pub(super) use diffs::thread as thread_diff;
pub(super) use listing::{claim, labels, list, own_space, unclaim};
