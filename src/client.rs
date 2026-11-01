//! What apps use Shoal through: the daemon's protocol, the types it carries,
//! and the client-side logic the CLI shares with them.

pub mod agent;
pub mod ai;
pub mod config;
pub mod demos;
pub mod model;
pub mod protocol;
pub mod removal;
pub mod repository;
pub mod reviews;
pub mod sessions;
pub mod sim;
pub mod state;
pub mod time;
pub mod validate;

pub use shoal_forge as forge;
