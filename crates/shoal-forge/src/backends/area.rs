//! The areas of what a forge does its own way, each a trait over
//! [`super::Backend`] that every forge implements in a module of its own.
mod activity;

pub(crate) use activity::Activity;
