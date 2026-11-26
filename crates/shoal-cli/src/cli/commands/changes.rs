//! `shoal changes` and `shoal diff --thread`.
mod diffs;
mod listing;

pub(super) use diffs::thread as thread_diff;
pub(super) use listing::{claim, labels, list, own_space, unclaim};
