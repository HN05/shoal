//! Shoal's retrying proxy between agents and their API upstream, from the
//! global `[proxy.<name>]` tables an `[ai.<name>]` tool names. The
//! `shoal-proxy` crate forwards and retries; Shoal configures and runs one
//! per agent launch.
mod config;
mod launch;

pub use config::{Proxies, Proxy, routes, validate, validate_references};
pub use launch::{
    KEY_HEADER, KEY_VARIABLE, Running, forget_reports, remember_executable, send_relayed, start,
};

#[cfg(test)]
mod tests;
