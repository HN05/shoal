//! Tracked execution wrapper. The CLI process launches the command inside the
//! workspace, hands it the terminal, forwards daemon stop requests, and reports
//! completion. The daemon never touches terminal I/O. A detached launch runs
//! the same wrapper in a background `shoal` process with a log file instead
//! of the terminal, so the invoking CLI returns once the launch is recorded.
mod cancellation;
mod completion;
mod containers;
mod demo;
mod input;
mod launch;
mod link;
mod mode;
mod own_group;
mod pause;
mod restore;
mod signal_report;
mod spawn;
mod supervise;
mod takeover;
mod wake;

use anyhow::{Context as _, Result, ensure};
use shoal_ids::space_ids::SpaceId;
use shoal_protocol::space_names::SpaceSelector;
use std::{ffi::OsString, path::PathBuf};

use crate::{client::connection::CommandPaths, env};
pub use cancellation::{cancel, cancellation};
pub use completion::SetupVerificationFailure;
use launch::{run_taken_over, run_tracked};
use mode::Mode;
pub use restore::{Restored, resolve};
use shoal_core::execution::{TrackedAgent, clear_terminal, recovery};

/// `agent` names the agent the command runs; the daemon tells the user when it exits.
pub async fn run(
    paths: &CommandPaths,
    workspace: SpaceSelector,
    command: Vec<OsString>,
    agent: Option<TrackedAgent>,
) -> Result<i32> {
    ensure!(!command.is_empty(), "a command is required after --");
    // The execution acts in its own right, never under what an approval of
    // the command that started it granted.
    crate::env::release_grant();
    let ran_agent = agent.is_some();
    let result = run_tracked(
        paths,
        workspace,
        command,
        Mode::Command { record: false },
        agent,
    )
    .await;
    if ran_agent {
        return result.map(env::session_exit_code);
    }
    result
}

/// A command the user chose, recorded when `shoal stop` interrupts it.
pub async fn run_command(
    paths: &CommandPaths,
    workspace: SpaceSelector,
    command: Vec<OsString>,
) -> Result<i32> {
    ensure!(!command.is_empty(), "a command is required after --");
    run_tracked(
        paths,
        workspace,
        command,
        Mode::Command { record: true },
        None,
    )
    .await
}

/// Consume the selected recovery records only after the replacement process is
/// recorded, so failed launches remain recoverable without duplicating sessions.
/// `source` says what made a restore the user runs here, which counts as
/// their interaction with the agent.
pub async fn run_recovery(
    paths: &CommandPaths,
    workspace: SpaceSelector,
    command: Vec<OsString>,
    agent: TrackedAgent,
    (records, source): (Vec<PathBuf>, Option<shoal_protocol::launches::LaunchSource>),
) -> Result<i32> {
    ensure!(!command.is_empty(), "a restore command is required");
    run_tracked(
        paths,
        workspace,
        command,
        Mode::Recovery { records, source },
        Some(agent),
    )
    .await
    .map(env::session_exit_code)
}

/// The background half of a detached launch: an ordinary tracked wrapper whose
/// child reads `/dev/null` and writes `log`. It stays alive as the execution's
/// wrapper, so stop requests, removal, and recovery see a connected command.
pub async fn run_detached_wrapper(
    paths: &CommandPaths,
    workspace: SpaceSelector,
    log: PathBuf,
    command: Vec<OsString>,
    agent: Option<TrackedAgent>,
) -> Result<i32> {
    ensure!(!command.is_empty(), "a command is required after --");
    run_tracked(paths, workspace, command, Mode::Detached { log }, agent).await
}

/// A demo's wrapper, which records `launch` with its execution and restarts
/// `command` until the demo is stopped; a stopped demo exits 0.
pub async fn run_demo(
    paths: &CommandPaths,
    workspace: SpaceSelector,
    launch: shoal_protocol::demos::Launch,
    command: Vec<OsString>,
) -> Result<i32> {
    ensure!(!command.is_empty(), "a command is required after --");
    clear_terminal();
    let mode = Mode::Demo {
        launch: Box::new(launch),
    };
    run_tracked(paths, workspace, command, mode, None)
        .await
        .map(|_| 0)
}

/// What an agent's wrapper whose binary an upgrade replaced hands the
/// installed one.
pub struct TakenOver {
    pub workspace: SpaceId,
    /// The agent, as the execution registered it, with the model and effort
    /// it runs with.
    pub agent: TrackedAgent,
    /// The descriptor of the wrapper's daemon connection.
    pub connection: i32,
    /// The recovery record the agent resumes from.
    pub record: Option<PathBuf>,
    /// The log a detached agent writes to.
    pub log: Option<PathBuf>,
}

/// Continue an agent's wrapper whose binary an upgrade replaced with this
/// one: register its execution again and restore the agent.
pub async fn take_over(paths: &CommandPaths, taken: TakenOver) -> Result<i32> {
    run_taken_over(paths, taken)
        .await
        .map(env::session_exit_code)
}

pub async fn publish(
    paths: &CommandPaths,
    workspace: SpaceSelector,
    command: Vec<OsString>,
) -> Result<i32> {
    run_tracked(paths, workspace, command, Mode::Publish, None).await
}

/// Merge the workspace's branch; its worker writes the result as a
/// [`MergeResult`] to `result`.
pub async fn merge(
    paths: &CommandPaths,
    workspace: SpaceSelector,
    options: crate::model::MergeOptions,
    result: PathBuf,
) -> Result<i32> {
    run_tracked(
        paths,
        workspace,
        vec![],
        Mode::Merge { options, result },
        None,
    )
    .await
}

/// What a merge worker writes: what it merged, or why it could not.
pub type MergeResult = std::result::Result<crate::model::MergedBranch, String>;

/// Undo the workspace's latest merge; its worker writes the result as an
/// [`UnmergeResult`] to `result`.
pub async fn unmerge(
    paths: &CommandPaths,
    workspace: SpaceSelector,
    result: PathBuf,
) -> Result<i32> {
    run_tracked(paths, workspace, vec![], Mode::Unmerge { result }, None).await
}

/// What an unmerge worker writes: what it undid, or why it could not.
pub type UnmergeResult = std::result::Result<crate::model::Unmerged, String>;

/// Run the workspace's setup command. An error from before it ran, as when
/// the daemon refused it or did not register it, carries [`SetupNotStarted`]
/// as its context.
pub async fn setup(paths: &CommandPaths, workspace: SpaceSelector) -> Result<i32> {
    let launch = launch::prepare_tracked_launch(paths, workspace, vec![], &Mode::Setup, None)
        .await
        .context(SetupNotStarted)?;
    launch.run_to_end(paths, &Mode::Setup).await
}

/// The context of an error that kept setup from starting.
#[derive(Debug)]
pub struct SetupNotStarted;

impl std::fmt::Display for SetupNotStarted {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("setup did not start")
    }
}

#[cfg(test)]
mod tests;
