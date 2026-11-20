//! Moves between machines joined to the same hub. The machine a space
//! moves from delivers its package over a duplex stream of the hub's links
//! to the destination's daemon, which continues it there as
//! `shoal restore --from -` would. The destination's claim of the space at
//! the hub commits the move, so a source that cannot learn from the
//! destination whether it claimed the space asks the hub instead.
mod receiving;
