//! The latest package of each space of an intermittent machine, which the
//! hub keeps so another machine can continue the space while its own is
//! gone. The machine sends a space's package once it changed, over a stream
//! of its link; the hub keeps one per space, in place of the one before,
//! until the space ends on that machine.
mod keeping;
mod sending;

pub(crate) use keeping::{Kept, Sent};
