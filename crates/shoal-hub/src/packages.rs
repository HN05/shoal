//! The latest package of each space of an intermittent machine, which the
//! hub keeps so another machine can continue the space while its own is
//! gone. The machine sends a space's package once it changed, over a stream
//! of its link; the hub keeps one per space, in place of the one before,
//! until the space ends on that machine. A machine shutting down hands its
//! spaces over the same way, and the hub continues each elsewhere.
mod announcing;
mod drops;
mod handing_over;
mod handovers;
mod keeping;
mod retiring;
mod sending;
mod sleeps;
pub(crate) mod takeovers;

pub(crate) use handovers::Handovers;
pub(crate) use keeping::{Continuation, Kept, Sent, forget};
pub(crate) use sleeps::{Sleeps, forget as forget_sleep};

#[cfg(test)]
mod tests;
