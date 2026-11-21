//! Human decisions authorize cooperative allocations; they never reserve capacity.
//! Its submodules store, route, word and deliver the requests subsystems
//! make.
mod delivery;
pub(crate) mod gated;
pub mod proposals;
mod routing;
mod store;
mod text;

pub use shoal_daemon_base::approval_requests::{ApprovalRequestExt, ReasonRequired};
pub(crate) use shoal_protocol::approvals::{
    ActionSpecification, ApprovalRequest, DecisionStatus, GateStep, HoldSpecification, MERGE,
    MergeSpecification, Specification, Target, Undecided, UnmergeSpecification,
};
pub(crate) use store::{
    by_key, by_number, list_of_space, oldest_for_user, pending_count, release_decided_by, renumber,
};
pub use store::{check, current, release, request_action, retire};

#[cfg(test)]
mod tests;
