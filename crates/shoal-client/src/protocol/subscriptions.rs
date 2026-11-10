use serde::{Deserialize, Serialize};

use super::Lifetime;
use crate::subscriptions::records::Topic;

/// Requests that follow records as they change.
#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SubscriptionMethod {
    /// Long-lived: the records of `topics` the caller may read, as one
    /// [`super::Body::Records`] snapshot, then each record that changes as
    /// [`super::Body::RecordChanged`] and each one removed as
    /// [`super::Body::RecordRemoved`], until the client hangs up. A scoped
    /// caller covers its workspace and those split from it, without
    /// notifications. With `log`, the entries of one space's log too.
    Subscribe {
        topics: Vec<Topic>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        log: Option<crate::log::LogTopic>,
    },
}

impl SubscriptionMethod {
    pub fn lifetime(&self) -> Lifetime {
        match self {
            Self::Subscribe { .. } => Lifetime::Open,
        }
    }
}
