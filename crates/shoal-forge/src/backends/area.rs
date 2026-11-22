//! The areas of what a forge does its own way, each a trait over
//! [`super::Backend`] that every forge implements in a module of its own.
mod activity;
mod branches;
mod conventions;
mod reviews;

pub(crate) use activity::Activity;
pub(crate) use branches::{BranchFields, Branches};
pub(crate) use conventions::Conventions;
pub(crate) use reviews::Reviews;
