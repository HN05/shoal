//! Shoal's retrying proxy between agents and their API upstream, from the
//! global `[proxy.<name>]` tables an `[ai.<name>]` tool names. The
//! `shoal-proxy` crate forwards and retries; Shoal configures and runs one
//! per agent launch.
mod config;
mod launch;

pub use config::{Proxies, upstreams, validate, validate_references};
pub use launch::{Running, URL, fill_url, start};

#[cfg(test)]
mod tests;
