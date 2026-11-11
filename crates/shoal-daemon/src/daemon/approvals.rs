//! Human decisions authorize cooperative allocations; they never reserve capacity.
//! This module extends the client's request and target types for the daemon;
//! its submodules store, route, word and deliver requests.
use crate::{client::space_ids::SpaceId, daemon::item_numbers::ItemNumber};

mod delivery;
pub mod proposals;
mod routing;
mod store;
mod text;

pub(crate) use crate::client::approvals::{
    ApprovalRequest, DecisionStatus, HoldSpecification, Lifetime, MERGE, MergeSpecification,
    PortSpecification, PushSpecification, ResourceSpecification, SecretSpecification,
    SimulatorSpecification, Specification, Target, Undecided, UnmergeSpecification,
};
pub use store::{ReasonRequired, check, current, release};
pub(crate) use store::{pending_count, release_decided_by};

/// Operations on the client type [`ApprovalRequest`] that only the `shoal` command uses.
pub(crate) trait ApprovalRequestExt {
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

#[cfg(test)]
mod tests;
