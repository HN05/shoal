//! The reviews the daemon runs: one-shot agent runs, started in the
//! background without a session or terminal.
use shoal_core::{ai, config, env, process};
use shoal_daemon_base::log;
use shoal_protocol::agent;

#[cfg(test)]
use shoal_core::test_support;

pub mod oneshot;

#[cfg(test)]
shoal_core::prepare_test_process!();
