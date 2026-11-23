//! Human decisions authorize cooperative allocations; they never reserve capacity.
//! Its submodules store, route, word and deliver the requests subsystems
//! make.
mod delivery;
pub mod gated;
pub mod proposals;
pub mod routing;
pub mod store;
pub mod text;

pub use shoal_daemon_base::approval_requests::{ApprovalRequestExt, ReasonRequired};
pub use shoal_protocol::approvals::{
    ActionSpecification, ApprovalRequest, DecisionStatus, GateStep, HoldSpecification, MERGE,
    MergeSpecification, Specification, Target, Undecided, UnmergeSpecification,
};
pub use store::{
    by_key, by_number, drop_worktree_requests, list_of_space, oldest_for_user, pending_count,
    release_decided_by, renumber,
};
pub use store::{check, current, release, request_action, retire};

#[cfg(test)]
mod tests;
