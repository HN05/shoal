//! Replacing the body of a comment.

use anyhow::{Result, ensure};
use serde::{Deserialize, Serialize};

use crate::numbers::CommentId;

/// A new body for a comment or inline comment.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CommentEdit {
    /// The ID of a comment or inline comment, as `shoal view` shows it.
    pub comment: CommentId,
    pub body: String,
}

impl CommentEdit {
    pub fn validate(&self) -> Result<()> {
        ensure!(!self.body.trim().is_empty(), "the comment is empty");
        Ok(())
    }
}
