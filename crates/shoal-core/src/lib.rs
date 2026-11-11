//! The code the CLI and the daemon share: Git, agents, executions,
//! processes, terminal hosts and the rest of what both reach.

// First, so that every module below sees its print macros.
#[macro_use]
pub mod stdout;

pub mod agent_git;
pub mod agents;
pub mod build;
pub mod caches;
pub mod config_file;
pub mod daemon_handoff;
pub mod daemon_owner;
pub mod env;
pub mod execution;
pub mod fsutil;
pub mod git;
mod git_credential;
pub mod git_profile;
mod happy;
pub mod hooks;
pub mod installed;
pub mod paths;
pub mod presets;
pub mod process;
pub mod proxy;
pub mod push_gate;
pub mod refusal;
pub mod releases;
pub mod removal;
pub mod repository;
pub mod secrets;
pub mod service;
pub mod sessions;
pub mod shell;
pub mod start_plan;
pub mod terminal;
#[cfg(any(test, feature = "test-support"))]
pub mod test_support;
pub mod tmp_dirs;
pub mod tools;
pub mod transfer;
pub mod workers;

use shoal_client as client;
use shoal_client::time;
use shoal_client::{agent, ai, config, demos, model, protocol, state};
use shoal_forge as forge;
use shoal_subprocess as subprocess;

/// Tokio worker threads get more stack than its 2 MiB default: the daemon
/// polls deep async paths, such as manual cleanup's removal, whose debug
/// builds overflow that on Linux. Unused stack is reserved, not committed.
pub const WORKER_STACK: usize = 8 << 20;

#[cfg(test)]
mod helper_tests {
    crate::helper_tests!();
}
