//! What a lease, hold or secret request prints when it gets nothing.
use serde::{Deserialize, Serialize};

use crate::{approvals::ApprovalRequest, leases::RefusalCode, output::outputs};

/// A request that acquired nothing: its capacity is busy, or it waits for
/// or was denied an approval.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NotAcquired {
    /// Always false.
    pub acquired: bool,
    /// Why, when it has a name.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub code: Option<RefusalCode>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
    /// A resource request's pool and member.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pool: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resource: Option<String>,
    /// The approval request it waits for or was denied.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub request: Option<Box<ApprovalRequest>>,
}

impl NotAcquired {
    /// Refused for `code`, with `message` saying what holds the capacity.
    pub fn busy(code: Option<RefusalCode>, message: &str) -> Self {
        Self {
            acquired: false,
            code,
            message: Some(message.to_owned()),
            pool: None,
            resource: None,
            request: None,
        }
    }
}

/// Whether a request released what it named.
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub struct Released {
    pub released: bool,
}

outputs!(NotAcquired, Released);
