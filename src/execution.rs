//! Tracked execution wrapper. The CLI process launches the command inside the
//! workspace, hands it the terminal, forwards daemon stop requests, and reports
//! completion. The daemon never touches terminal I/O. A detached launch runs
//! the same wrapper in a background `shoal` process with a log file instead
//! of the terminal, so the invoking CLI returns once the launch is recorded.
mod attachment;
mod cancellation;
mod completion;
mod demo;
mod detached;
mod launch;
mod link;
mod mode;
mod pause;
pub mod recovery;
mod spawn;
mod supervise;
mod takeover;
mod terminal;
mod wake;

use crate::client::space_ids::SpaceId;
use crate::client::space_names::SpaceSelector;
use anyhow::{Result, ensure};
use std::{ffi::OsString, path::PathBuf};

use crate::{env, paths::Paths};
pub use cancellation::{cancel, cancellation};
pub use completion::SetupVerificationFailure;
pub use detached::{DetachedLaunch, launch_detached};
use launch::{run_taken_over, run_tracked};
use mode::Mode;
pub use supervise::exit_code;
use terminal::clear_terminal;
pub(crate) use terminal::{reset_input_modes, run_in_terminal};

/// An agent a tracked wrapper runs: its name, as `--agent` names it, the
/// model and effort it runs with, and the mode it started in, which its
/// restores keep.
#[derive(Debug, Clone)]
pub struct TrackedAgent {
    pub name: crate::agent::Agent,
    pub model: crate::agent::ModelChoice,
    pub mode: crate::config::modes::ModeName,
    /// The permissions `--permission` gives its agent, whatever its mode's.
    pub narrowing: crate::config::permissions::PermissionsConfig,
}

#[cfg(test)]
impl TrackedAgent {
    /// Agent `name` running with the tool's own model and effort.
    pub fn named(name: impl Into<crate::agent::Agent>) -> Self {
        Self {
            narrowing: Default::default(),
            name: name.into(),
            model: Default::default(),
            mode: crate::config::modes::ModeName::DEFAULT,
        }
    }
}

/// `agent` names a Shoal agent shortcut; the daemon tells the user when it exits.
pub async fn run(
    paths: &Paths,
    workspace: SpaceSelector,
    command: Vec<OsString>,
    agent: Option<TrackedAgent>,
) -> Result<i32> {
    ensure!(!command.is_empty(), "a command is required after --");
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
    paths: &Paths,
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
pub async fn run_recovery(
    paths: &Paths,
    workspace: SpaceSelector,
    command: Vec<OsString>,
    agent: TrackedAgent,
    records: Vec<PathBuf>,
) -> Result<i32> {
    ensure!(!command.is_empty(), "a restore command is required");
    run_tracked(
        paths,
        workspace,
        command,
        Mode::Recovery { records },
        Some(agent),
    )
    .await
    .map(env::session_exit_code)
}

/// The background half of a detached launch: an ordinary tracked wrapper whose
/// child reads `/dev/null` and writes `log`. It stays alive as the execution's
/// wrapper, so stop requests, removal, and recovery see a connected command.
pub async fn run_detached_wrapper(
    paths: &Paths,
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
    paths: &Paths,
    workspace: SpaceSelector,
    launch: crate::demos::Launch,
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
pub async fn take_over(paths: &Paths, taken: TakenOver) -> Result<i32> {
    run_taken_over(paths, taken)
        .await
        .map(env::session_exit_code)
}

pub async fn publish(
    paths: &Paths,
    workspace: SpaceSelector,
    command: Vec<OsString>,
) -> Result<i32> {
    run_tracked(paths, workspace, command, Mode::Publish, None).await
}

/// Merge the workspace's branch; its worker writes the result as a
/// [`MergeResult`] to `result`.
pub async fn merge(
    paths: &Paths,
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
pub async fn unmerge(paths: &Paths, workspace: SpaceSelector, result: PathBuf) -> Result<i32> {
    run_tracked(paths, workspace, vec![], Mode::Unmerge { result }, None).await
}

/// What an unmerge worker writes: what it undid, or why it could not.
pub type UnmergeResult = std::result::Result<crate::model::Unmerged, String>;

pub async fn setup(paths: &Paths, workspace: SpaceSelector) -> Result<i32> {
    run_tracked(paths, workspace, vec![], Mode::Setup, None).await
}

#[cfg(test)]
mod tests;
