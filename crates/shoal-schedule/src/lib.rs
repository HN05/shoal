//! The order the agent queue starts its waiting work in: how much work
//! matters, and the rank that orders it. The daemon builds what the order
//! reads from its own state; nothing here reads that state.

pub mod priority;
pub mod rank;
