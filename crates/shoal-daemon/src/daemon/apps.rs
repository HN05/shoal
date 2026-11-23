//! Answering apps' requests of the app store, which `shoal-apps` keeps, on
//! the daemon's socket and across the hub: writes whose bytes follow the
//! request, watches that go on while their caller's scope holds, and the
//! shared entries a machine that joined a hub reaches there; and the config
//! layers apps read their settings in.
mod hub;
mod repositories;
mod socket;

pub(crate) use socket::{Keeper, Socket};
