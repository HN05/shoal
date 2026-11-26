//! The order the agent queue starts and stops work in, and the machine a hub
//! places a new space on. Each policy reads only the snapshot its caller
//! builds, never the daemon's or the hub's state.

pub mod leads;
pub mod placement;
pub mod priority;
pub mod rank;
pub mod ranking;
pub mod scheduler;
pub mod snapshot;
pub mod stopping;
