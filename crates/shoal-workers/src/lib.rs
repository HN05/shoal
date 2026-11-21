//! The execution wrapper and the workers: the code that runs in the
//! processes Shoal starts and calls the daemon. Core's `workers` module
//! builds those processes' command lines for the daemon, which never
//! depends on this crate.

pub mod attach;
pub mod execution;
pub mod following;
pub mod git_credential;
pub mod homes;
pub mod moves;
pub mod opened_pull;
pub mod operations;
pub mod places;
pub mod preparation;
pub mod proxy;
pub mod push_gate;
pub mod queued_launches;
pub mod queued_starts;
pub mod resume;
pub mod secrets;
pub mod settings;

use shoal_client as client;
use shoal_core::{
    agent_git, agents, ai, caches, config, env, fsutil, git, hooks, installed, paths, process,
    releases, repository, scoping, sessions, shell, start_plan, terminal, tmp_dirs, tools, workers,
    workspace_context,
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
