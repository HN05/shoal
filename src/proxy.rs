//! Shoal's retrying proxy between agents and their API upstream, from the
//! global `[machine.proxies.<name>]` tables an `[machine.agents.<name>]` entry names. The
//! `shoal-proxy` crate forwards and retries; Shoal configures and runs one
//! per agent launch.
mod launch;

pub use crate::config::proxies::Proxy;
pub use launch::{
    KEY_HEADER, KEY_VARIABLE, Running, forget_reports, send_relayed, send_report, start,
};

#[cfg(test)]
mod tests;
