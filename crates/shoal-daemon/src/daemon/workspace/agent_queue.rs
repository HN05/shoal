//! The agent queue: `shoal start` launches past `max_agents`, restores of
//! stopped agents and one-shot runs such as reviews wait here and start in
//! order as slots free.
mod actions;
mod agents;
pub(crate) mod asleep;
mod blockers;
mod dispatch;
mod entries;
mod expiry;
mod limits;
mod rank;
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

pub(crate) use dispatch::Start;
pub(crate) use state::{AgentQueue, OneShotAdmission, OneShotStop, Serving};
pub(crate) use store::last_seq;
pub(crate) use wake::Grant;
pub(crate) use worker::HasAgent;

#[cfg(test)]
pub(crate) mod tests;
