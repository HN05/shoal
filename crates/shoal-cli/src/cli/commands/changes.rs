//! `shoal changes` and the hook that records them: the hunks Shoal records
//! as the agents it runs change the files of their worktrees.
mod hook;

pub(super) use hook::report;
