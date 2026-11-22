//! The agent queue: `shoal start` launches past `max_heavy_agents`, restores of
//! stopped agents and one-shot runs such as reviews wait here and start in
//! order as slots free.
pub mod actions;
pub mod agents;
pub mod asleep;
pub mod blockers;
pub mod carrier;
pub mod chats;
pub mod claims;
pub mod dispatch;
pub mod entries;
pub mod expiry;
pub mod failures;
pub mod limits;
pub mod openings;
pub mod ranking;
pub mod restores;
pub mod room;
pub mod setup_waits;
pub mod slots;
pub mod state;
pub mod stopped;
pub mod store;
pub mod wake;
pub mod weight;
pub mod worker;

pub use blockers::Readiness;
pub use dispatch::Start;
pub use state::{AgentQueue, OneShotAdmission, OneShotStop, RestoreLink, Serving};
pub use store::last_seq;
pub use wake::Grant;
pub use worker::HasStoppedAgent;
