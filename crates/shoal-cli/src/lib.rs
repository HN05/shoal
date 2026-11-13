//! Shoal's CLI: the `shoal` command's arguments and commands. The `shoal`
//! command passes in the daemon its `shoal daemon run` runs, which this crate
//! does not build.

// Every module sees the shared crate's print macros.
#[macro_use]
extern crate shoal_core;

mod cli;

#[cfg(test)]
use shoal_core::test_support;
use shoal_core::{
    agent_git, agents, build, config_file, daemon_handoff, daemon_owner, env, execution, fsutil,
    git, git_credential, installed, opened_pull, operations, paths, process, profile, proxy,
    push_gate, queued_launches, refusal, releases, removal, repository, secrets, service, sessions,
    shell, start_plan, stdout, terminal, tools, transfer, workers,
};

use shoal_client as client;
use shoal_client::time;
use shoal_client::{agent, ai, config, demos, model, protocol, reviews, state, validate};
use shoal_core::WORKER_STACK;
use shoal_forge as forge;
use shoal_subprocess as subprocess;

pub use cli::entry::{RunDaemon, run};

#[cfg(test)]
mod helper_tests {
    shoal_core::helper_tests!();
}
