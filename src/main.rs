//! The `shoal` command: the CLI and the daemon, over [`shoal_core`] and
//! [`shoal_client`].

// Every module sees the shared crate's print macros.
#[macro_use]
extern crate shoal_core;

mod cli;
mod daemon;
mod seed;

#[cfg(test)]
use shoal_core::test_support;
use shoal_core::{
    agent_git, agents, build, caches, config_file, daemon_handoff, daemon_owner, env, execution,
    fsutil, git, git_profile, hooks, installed, paths, presets, process, proxy, push_gate, refusal,
    releases, removal, repository, secrets, service, sessions, shell, start_plan, stdout, terminal,
    tmp_dirs, tools, transfer, workers,
};

use shoal_client as client;
use shoal_client::time;
use shoal_client::{agent, ai, config, demos, model, profile, protocol, reviews, state, validate};
use shoal_core::WORKER_STACK;
use shoal_forge as forge;
use shoal_subprocess as subprocess;

fn main() {
    cli::entry::run(|paths, managed, handoff| Box::pin(daemon::run(paths, managed, handoff)));
}

#[cfg(test)]
mod helper_tests {
    shoal_core::helper_tests!();
}
