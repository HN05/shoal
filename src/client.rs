//! What apps use Shoal through: the daemon's protocol, the types it carries,
//! and the client-side logic the CLI shares with them.

pub mod ai;
pub mod config;
pub mod model;
pub mod protocol;
pub mod repository;
pub mod state;
pub mod time;
pub mod validate;

pub use shoal_forge as forge;
