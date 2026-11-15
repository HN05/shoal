//! The order the queue starts its waiting work in, read from a snapshot
//! of the daemon's state that the daemon builds: the policy that decides
//! it, and the ranking the built-in schedules share.

pub mod leads;
pub mod ranking;
pub mod scheduler;
pub mod snapshot;
