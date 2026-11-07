//! The agent queue: `shoal start` launches past `max_agents`, restores of
//! stopped agents and one-shot runs such as reviews wait here and start in
//! order as slots free.
mod actions;
mod agents;
mod dependencies;
mod dispatch;
mod entries;
mod expiry;
mod limits;
mod order;
mod restores;
mod slots;
mod state;
mod stopped;
mod store;
mod wake;

pub(crate) use dispatch::Start;
pub(crate) use order::place_entry;
pub(crate) use state::{AgentQueue, OneShotSlot, OneShotStop};
pub(crate) use wake::Grant;

#[cfg(test)]
mod tests;
