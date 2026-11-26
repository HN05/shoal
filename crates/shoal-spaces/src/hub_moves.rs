//! Moves between machines joined to the same hub. See `docs/design/moves.md`
//! and `docs/design/hub.md`.
pub mod after_turn;
pub mod authorization;
mod delivering;
mod handed_over;
mod handing_over;
pub mod held;
pub mod receiving;
mod removed_elsewhere;
mod removed_records;

pub use authorization::moves_without_permission;
pub use receiving::Receiving;
