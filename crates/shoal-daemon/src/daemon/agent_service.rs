//! What the agents' service needs of the rest of the daemon, which the
//! daemon implements here.
mod alerts;
mod conversions;
mod demos;
mod handovers;
mod leases;
mod lifecycles;
mod links;
mod memory;
mod merges;
mod operations;
mod ranking;
mod records;
mod repositories;
mod reviews;
mod runner;
mod spaces;
mod turns;

pub(crate) use records::{AgentRecords, SpaceRemovals};

#[cfg(test)]
pub(crate) mod tests;
