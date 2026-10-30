//! A typed, stable API to Shoal's daemon for apps that are not Shoal's CLI.
//! It is served on the daemon's socket beside Shoal's internal protocol and
//! versioned with this crate; `README.md` describes the protocol and its
//! compatibility rule.
mod client;
mod errors;
mod following;
mod methods;
mod open_enum;
mod operations;
pub mod records;
pub mod schema;
mod socket;
mod subscription;
pub mod wire;

pub use client::{Client, Connector};
pub use errors::{ApiError, Error, ErrorCode};
pub use following::{Followed, Following};
pub use methods::*;
pub use operations::*;
pub use records::Record;
pub use socket::{STATE_DIR_VARIABLE, default_socket, socket_in};
pub use subscription::{Subscription, Update};

/// The API version this crate speaks, which is its own version.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");
