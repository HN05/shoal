//! The execution wrapper and the workers: the code that runs in the
//! processes Shoal starts and calls the daemon. Core's `workers` module
//! builds those processes' command lines for the daemon, which never
//! depends on this crate.

// Every module sees the shared crate's print macros.
#[macro_use]
extern crate shoal_core;

pub mod chat_starts;
pub mod execution;
pub mod git_credential;
pub mod homes;
pub mod opened_pull;
pub mod operations;
pub mod places;
pub mod proxy;
pub mod push_gate;
pub mod queued_launches;
pub mod queued_starts;
pub mod secrets;
pub mod settings;

use shoal_client as client;
use shoal_core::{
    agent_git, agents, ai, caches, chat_handoff, config, env, fsutil, git, hooks, installed, paths,
    process, releases, repository, sessions, start_plan, terminal, tmp_dirs, tools, workers,
};
use shoal_forge_types as forge;
use shoal_protocol::{agent, model, protocol, state, time};

#[cfg(test)]
use shoal_core::test_support;

#[cfg(test)]
mod helper_tests {
    shoal_core::helper_tests!();
}
#[cfg(test)]
shoal_core::prepare_test_process!();
