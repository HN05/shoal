//! The order the agent queue starts its waiting work in: how much work
//! matters, the rank that explains each place, and the policy that decides
//! the order from a snapshot the daemon builds of its state; and the order
//! the same policy stops running work in to free resources. Nothing here
//! reads that state, and admission, which the daemon decides under its
//! own machine's limits, takes entries in the order a policy returns.

pub mod leads;
pub mod priority;
pub mod rank;
pub mod ranking;
pub mod scheduler;
pub mod snapshot;
pub mod stopping;
