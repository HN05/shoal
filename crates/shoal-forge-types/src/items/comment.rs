//! Replacing the body of a comment.

use anyhow::{Result, ensure};
use serde::{Deserialize, Serialize};

use crate::{items::attach::Attachment, numbers::CommentId};

/// A new body for a comment or inline comment.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CommentEdit {
    /// The ID of a comment or inline comment, as `shoal view` shows it.
    pub comment: CommentId,
    /// Without one, the attachments go after the comment's current body.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub body: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub attachments: Vec<Attachment>,
}

impl CommentEdit {
    pub fn validate(&self) -> Result<()> {
        match &self.body {
            Some(body) => ensure!(
                !body.trim().is_empty() || !self.attachments.is_empty(),
                "the comment is empty"
            ),
            None => ensure!(
                !self.attachments.is_empty(),
                "nothing to change; pass a new body or an attachment"
            ),
        }
        Ok(())
    }
}
