//! The execution wrapper and the workers: the code that runs in the
//! processes Shoal starts and calls the daemon. Core's `workers` module
//! builds those processes' command lines for the daemon, which never
//! depends on this crate.

pub mod execution;
pub mod git_credential;
pub mod operations;
pub mod proxy;
pub mod push_gate;
pub mod secrets;

use shoal_client as client;
use shoal_core::{
    agent_git, agents, ai, config, env, fsutil, git, hooks, installed, opened_pull, paths, process,
    queued_launches, releases, sessions, terminal, tools, workers,
};
use shoal_forge_types as forge;
use shoal_protocol::{agent, model, protocol, state};

#[cfg(test)]
use shoal_core::test_support;
#[cfg(test)]
use shoal_protocol::time;

#[cfg(test)]
mod helper_tests {
    shoal_core::helper_tests!();
}
#[cfg(test)]
shoal_core::forget_runner_repository!();
