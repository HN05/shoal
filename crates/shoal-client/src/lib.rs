//! A typed, stable API to Shoal's daemon for apps that are not Shoal's CLI.
//! It is served on the daemon's socket beside Shoal's internal protocol and
//! versioned with this crate; `README.md` describes the protocol and its
//! compatibility rule.
mod errors;
mod methods;
mod open_enum;
pub mod records;
pub mod schema;
pub mod wire;

pub use errors::{ApiError, Error, ErrorCode};
pub use methods::*;
pub use records::Record;

/// The API version this crate speaks, which is its own version.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");
