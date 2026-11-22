//! What the daemon gives the agents' service: the spaces agents run in,
//! their settings, lifecycles and hooks, the repositories whose paths they
//! trust, the alerts that tell of them, what else ends with an agent's
//! turn, the operations, merges, demos, links, CI jobs, leases, reviews and
//! conversions an agent's work meets, the memory it takes, the snapshot the
//! queue ranks, and the records of other subsystems it reads and writes.
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

pub(crate) use records::AgentRecords;

#[cfg(test)]
pub(crate) mod tests;
