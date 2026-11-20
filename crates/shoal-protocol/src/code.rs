//! Code references: places in a worktree or at a commit that a post in a
//! thread points at, and how they follow the code as it changes.
use shoal_states::states;

mod lines;

pub use lines::{Followed, Lines, follow};

states!(
    /// What became of a reference's code since it was made.
    CodeState {
        /// Nothing inside it changed; it may have moved.
        Unchanged => "unchanged",
        /// Lines inside it were edited, and it covers what replaced them.
        Changed => "changed",
        /// Its code is gone, deleted or moved to another file; it keeps the
        /// point where the code was and its snippet.
        Outdated => "outdated",
    }
);
