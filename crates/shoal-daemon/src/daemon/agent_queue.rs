//! The agent queue: `shoal start` launches past `max_heavy_agents`, restores of
//! stopped agents and one-shot runs such as reviews wait here and start in
//! order as slots free.
pub(crate) mod actions;
mod agents;
pub(crate) mod asleep;
mod blockers;
mod carrier;
mod chats;
mod claims;
mod dispatch;
mod entries;
mod expiry;
mod failures;
mod limits;
mod openings;
mod ranking;
mod restores;
mod setup_waits;
mod slots;
mod state;
mod stopped;
mod store;
mod wake;
mod weight;
mod worker;

pub(crate) use blockers::Readiness;
pub(crate) use dispatch::Start;
pub(crate) use state::{AgentQueue, OneShotAdmission, OneShotStop, RestoreLink, Serving};
pub(crate) use store::last_seq;
pub(crate) use wake::Grant;

#[cfg(test)]
pub(crate) mod tests;
