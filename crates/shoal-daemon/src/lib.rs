//! Shoal's daemon, which the `shoal` command runs as `shoal daemon run`, and
//! the seed builds it keeps.

mod daemon;
mod seed;

#[cfg(test)]
use shoal_core::test_support;
use shoal_core::{
    agents, ai, build, caches, config, config_file, daemon_handoff, daemon_owner, env, execution,
    fsutil, git, git_profile, hooks, installed, paths, presets, process, profile, refusal,
    releases, removal, repository, reviews, secrets, service, sessions, shell, start_plan,
    terminal, tmp_dirs, tools, transfer,
};
// Tests stand in for the worker processes the daemon starts.
#[cfg(not(test))]
use shoal_core::workers;

use shoal_client as client;
use shoal_client::time;
use shoal_client::{agent, demos, model, protocol, state, validate};
use shoal_forge as forge;
use shoal_subprocess as subprocess;

/// Run the daemon in this process, given its state, whether its service
/// manager started it and the handoff from the daemon it replaces. The future
/// is boxed here, where the daemon's futures are laid out: laying them out in
/// another crate, as the `shoal` binary would, exceeds rustc's query depth
/// limit.
pub fn run(
    paths: paths::Paths,
    managed: bool,
    handoff: Option<daemon_handoff::Handoff>,
) -> futures_util::future::LocalBoxFuture<'static, anyhow::Result<()>> {
    Box::pin(daemon::run(paths, managed, handoff))
}

#[cfg(test)]
mod helper_tests {
    shoal_core::helper_tests!();
}
#[cfg(test)]
shoal_core::forget_runner_repository!();
