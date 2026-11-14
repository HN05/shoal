//! The repository's labels as the forge lists them, so agents can follow
//! the scheme the repository describes in its label descriptions.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Label {
    pub name: String,
    pub description: String,
    /// Whether an item takes at most one label of its scope: Forgejo's
    /// exclusive labels and GitLab's scoped `key::value` labels.
    pub exclusive: bool,
}
