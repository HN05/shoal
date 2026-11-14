//! Reactions on an issue, a PR or one of their comments, and changes to the
//! acting account's own.
pub mod allowed;

use anyhow::{Result, ensure};
use serde::{Deserialize, Serialize};

use crate::numbers::CommentId;

/// Adding or removing the acting account's reaction on an item, or on one
/// of its comments.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReactionChange {
    /// The ID of a comment or inline comment, as `shoal view` shows it.
    pub comment: Option<CommentId>,
    /// GitHub's name for the reaction, or the forge's own.
    pub content: String,
    pub remove: bool,
}

impl ReactionChange {
    pub fn validate(&self) -> Result<()> {
        ensure!(
            !self.content.is_empty() && !self.content.contains(char::is_whitespace),
            "name the reaction, such as +1 or heart"
        );
        Ok(())
    }
}

/// The reactions on an item or comment with one content, as a view lists
/// them.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Reaction {
    /// GitHub's name for it, such as `+1` or `heart`; GitLab's name for an
    /// emoji GitHub has no reaction for.
    pub content: String,
    pub count: u64,
    /// Whether the login the view reads as is among them.
    pub reacted: bool,
}
