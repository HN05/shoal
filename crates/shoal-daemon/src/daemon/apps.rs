//! Answering apps' requests of the app store, which `shoal-apps` keeps, on
//! the daemon's socket and across the hub.
mod hub;
mod repositories;
mod socket;

pub(crate) use socket::{Keeper, Socket};
