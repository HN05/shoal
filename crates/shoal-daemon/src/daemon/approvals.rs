//! Human decisions authorize cooperative allocations; they never reserve capacity.
//! Its submodules store, route, word and deliver the requests subsystems
//! make.
mod delivery;
pub(crate) mod gated;
pub mod proposals;
mod routing;
mod store;
mod text;

pub(crate) use routing::governing;
pub use shoal_daemon_base::approval_requests::ApprovalRequestExt;
pub(crate) use shoal_protocol::approvals::{
    ActionSpecification, ApprovalRequest, DecisionStatus, GateStep, HoldSpecification, MERGE,
    MergeSpecification, PortSpecification, PushSpecification, ResourceSpecification,
    SecretSpecification, SimulatorSpecification, Specification, Target, Undecided,
    UnmergeSpecification,
};
pub use store::{ReasonRequired, check, current, release, request_action};
pub(crate) use store::{pending_count, release_decided_by, renumber};

#[cfg(test)]
mod tests;
