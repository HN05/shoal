//! The default branch's sync with its remote.
use super::PulledBranch;
use crate::repository_ids::RepositoryId;
use serde::{Deserialize, Serialize};

/// How the background sync of a repository's default branch went.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CheckoutSync {
    pub repository_id: RepositoryId,
    /// When the default branch last followed its remote.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub synced_at: Option<crate::time::UnixTime>,
    /// Since when every sync failed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub failing_since: Option<crate::time::UnixTime>,
    /// The error of the last failed sync.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

/// A repository's remote fetched and its local default branch fast-forwarded.
#[derive(Debug, Serialize, Deserialize)]
pub struct SyncedRepository {
    pub repository_id: RepositoryId,
    /// The default branch's upstream remote, absent when it has none.
    pub remote: Option<String>,
    pub default_branch: PulledBranch,
}
