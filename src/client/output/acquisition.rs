//! What a lease, hold or secret request prints when it gets nothing.
use serde::{Deserialize, Serialize};

use crate::client::{approvals::ApprovalRequest, output::outputs};

/// A request that acquired nothing: its capacity is busy, or it waits for
/// or was denied an approval.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NotAcquired {
    /// Always false.
    pub acquired: bool,
    /// Why, such as `resource_busy` or `approval_pending`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub code: Option<String>,
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
    /// Busy capacity, with `code` naming what was busy when it says.
    pub fn busy(code: Option<&str>, message: &str) -> Self {
        Self {
            acquired: false,
            code: code.map(str::to_owned),
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
