//! Review threads on a PR: an inline comment and the replies to it, and
//! whether the forge marks them resolved.

use serde::{Deserialize, Serialize};

use crate::numbers::CommentId;

/// A review thread, found from any of its comments.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Thread {
    /// The comment that starts the thread.
    pub first: CommentId,
    /// The comments after the first.
    pub replies: usize,
    /// Unknown where the forge does not report it.
    pub resolved: Option<bool>,
    pub target: Target,
}

/// What a forge's reply and resolution requests name besides the first comment.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Target {
    /// The GraphQL node its resolution mutations take.
    GitHub {
        node: String,
    },
    GitLab {
        discussion: String,
    },
    Bitbucket,
    /// A reply is a comment of the thread's review on the first comment's
    /// lines: `line` starts them, negative on the old side.
    Forgejo {
        review: u64,
        path: String,
        line: i64,
        extra_lines: i64,
    },
}
