//! Building the approval requests subsystems make, which the approvals
//! store records, routes and decides, and its refusal of one without a
//! reason.
use crate::item_numbers::ItemNumber;
use shoal_ids::space_ids::SpaceId;
use shoal_protocol::approvals::{ApprovalRequest, DecisionStatus, Specification, Target};
use shoal_protocol::config::repo::Lifetime;
use std::fmt;

/// Operations on the client type [`ApprovalRequest`] that only the `shoal` command uses.
pub trait ApprovalRequestExt {
    fn new(
        owner: &SpaceId,
        target: Target,
        name: &str,
        specification: Specification,
        lifetime: Lifetime,
        reason: Option<&str>,
    ) -> Self;

    /// Whether `other` is this request as recorded, though its decision may
    /// differ: the same key, workspace, target, settings, reason and time.
    fn is_same(&self, other: &ApprovalRequest) -> bool;
}

impl ApprovalRequestExt for ApprovalRequest {
    fn new(
        owner: &SpaceId,
        target: Target,
        name: &str,
        specification: Specification,
        lifetime: Lifetime,
        reason: Option<&str>,
    ) -> Self {
        Self {
            number: ItemNumber::default(),
            key: String::new(),
            space_id: owner.clone(),
            space: None,
            space_removed: false,
            repository_id: None,
            target,
            name: name.into(),
            specification,
            lifetime,
            reason: reason.unwrap_or_default().into(),
            status: DecisionStatus::Pending,
            created_at: crate::time::UnixTime::now(),
            decided_at: None,
            active: true,
            routed: false,
            decider: None,
            url: None,
            warnings: Vec::new(),
            operation: None,
            gate: None,
        }
    }

    fn is_same(&self, other: &ApprovalRequest) -> bool {
        self.number == other.number
            && self.key == other.key
            && self.space_id == other.space_id
            && self.target == other.target
            && self.name == other.name
            && self.specification == other.specification
            && self.lifetime == other.lifetime
            && self.reason == other.reason
            && self.created_at == other.created_at
    }
}

/// A new request names no reason; callers may say how to give one.
#[derive(Debug)]
pub struct ReasonRequired;

impl fmt::Display for ReasonRequired {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("approval requires --reason explaining the request")
    }
}

impl std::error::Error for ReasonRequired {}
