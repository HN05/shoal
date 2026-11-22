//! Moves between machines joined to the same hub. The machine a space
//! moves from delivers its package over a duplex stream of the hub's links
//! to the destination's daemon, which continues it there as
//! `shoal restore --from -` would. The destination's claim of the space at
//! the hub commits the move, so a source that cannot learn from the
//! destination whether it claimed the space asks the hub instead. A
//! removed space's record moves the same way, without an agent to stop.
//! A space a machine handed over as it shut down arrives from the hub's
//! package of it instead.
mod after_turn;
mod authorization;
mod delivering;
mod handed_over;
mod held;
mod receiving;
mod removed_records;

pub(crate) use authorization::moves_without_permission;
pub(crate) use receiving::Receiving;
