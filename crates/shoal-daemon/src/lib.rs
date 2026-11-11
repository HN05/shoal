//! Shoal's daemon, which the `shoal` command runs as `shoal daemon run`, and
//! the seed builds it keeps.

mod daemon;
mod seed;

#[cfg(test)]
use shoal_core::test_support;
use shoal_core::{
    agents, build, caches, config_file, daemon_handoff, daemon_owner, env, execution, fsutil, git,
    git_profile, hooks, installed, paths, presets, process, refusal, releases, removal, repository,
    secrets, service, sessions, start_plan, terminal, tmp_dirs, tools, transfer,
};
// Tests stand in for the worker processes the daemon starts.
#[cfg(not(test))]
use shoal_core::workers;

use shoal_client as client;
use shoal_client::time;
use shoal_client::{agent, ai, config, demos, model, profile, protocol, reviews, state, validate};
use shoal_forge as forge;
use shoal_subprocess as subprocess;

pub use daemon::run;

#[cfg(test)]
mod helper_tests {
    shoal_core::helper_tests!();
}
