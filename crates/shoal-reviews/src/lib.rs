//! The local reviews the daemon runs, each a one-shot agent run started in
//! the background without a session or terminal, and the records of what
//! they found and how their workspace's agent answered it. What reviews
//! need of the rest of the daemon is the interface in [`daemon`].
use shoal_core::{ai, config, env, process, reviews};
use shoal_daemon_base::log;
use shoal_protocol::agent;

#[cfg(test)]
use shoal_core::test_support;

pub mod daemon;
pub mod oneshot;
#[cfg(test)]
mod stand_in;
pub mod store;

#[cfg(test)]
shoal_core::prepare_test_process!();
