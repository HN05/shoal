//! The areas of what a forge does its own way, each a trait over
//! [`super::Backend`] that every forge implements in a module of its own.
mod attachments;
mod checks;
mod conventions;
mod reactions;
mod runners;
mod runs;

pub(crate) use attachments::Attachments;
pub(super) use attachments::{image, link, served_at};
pub(crate) use checks::Checks;
pub(crate) use conventions::Conventions;
pub(crate) use reactions::Reactions;
pub(crate) use runners::Runners;
pub(crate) use runs::Runs;
