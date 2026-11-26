//! The latest package of each space of an intermittent machine, which the
//! hub keeps so another machine can continue the space while its own is
//! gone.
mod announcing;
mod drops;
mod handing_over;
mod handovers;
mod keeping;
mod retiring;
mod sending;
mod sleeps;
pub(crate) mod takeovers;

pub(crate) use handing_over::OwnReleases;
pub(crate) use handovers::Handovers;
pub(crate) use keeping::{Continuation, Kept, Sent, forget};
pub(crate) use sleeps::{Sleeps, forget as forget_sleep};

#[cfg(test)]
pub(crate) mod tests;
