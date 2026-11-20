//! What watches keep: the running wait of each space, each item's record on
//! a space's watch cursor, and what a branch's local merge check reports.
pub mod branch_report;
pub mod changes;
pub mod cursor;
mod waits;

pub use waits::{ActiveWait, ActiveWaits};
