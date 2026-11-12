//! The areas of what a forge does its own way, each a trait over
//! [`super::Backend`] that every forge implements in a module of its own.
mod checks;
mod conventions;
mod runners;
mod runs;

pub(crate) use checks::Checks;
pub(crate) use conventions::Conventions;
pub(crate) use runners::Runners;
pub(crate) use runs::Runs;
