//! Tracked execution wrapper. The CLI process launches the command inside the
//! workspace, hands it the terminal, forwards daemon stop requests, and reports
//! completion. The daemon never touches terminal I/O. A detached launch runs
//! the same wrapper in a background `shoal` process with a log file instead
//! of the terminal, so the invoking CLI returns once the launch is recorded.
mod attachment;
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

use anyhow::{Result, ensure};
use std::{ffi::OsString, path::PathBuf};

use crate::{env, paths::Paths};
pub use completion::SetupVerificationFailure;
pub use detached::{DetachedLaunch, launch_detached};
use launch::{run_taken_over, run_tracked};
use mode::Mode;
pub use supervise::exit_code;
use terminal::clear_terminal;
pub(crate) use terminal::{Terminal, reset_input_modes, run_in_terminal};

/// `agent` names a Shoal agent shortcut; the daemon tells the user when it exits.
pub async fn run(
    paths: &Paths,
    workspace: String,
    command: Vec<OsString>,
    agent: Option<String>,
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
pub async fn run_command(paths: &Paths, workspace: String, command: Vec<OsString>) -> Result<i32> {
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
    workspace: String,
    command: Vec<OsString>,
    agent: String,
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
    workspace: String,
    log: PathBuf,
    command: Vec<OsString>,
    agent: Option<String>,
) -> Result<i32> {
    ensure!(!command.is_empty(), "a command is required after --");
    run_tracked(paths, workspace, command, Mode::Detached { log }, agent).await
}

/// A demo's wrapper, which records `launch` with its execution and restarts
/// `command` until the demo is stopped; a stopped demo exits 0.
pub async fn run_demo(
    paths: &Paths,
    workspace: String,
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
    pub workspace: String,
    /// The agent, as the execution registered it.
    pub agent: String,
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

pub async fn publish(paths: &Paths, workspace: String, command: Vec<OsString>) -> Result<i32> {
    run_tracked(paths, workspace, command, Mode::Publish, None).await
}

/// Land the workspace's branch; its worker writes the result as a
/// [`LandResult`] to `result`.
pub async fn land(paths: &Paths, workspace: String, push: bool, result: PathBuf) -> Result<i32> {
    run_tracked(paths, workspace, vec![], Mode::Land { push, result }, None).await
}

/// What a landing worker writes: what it landed, or why it could not.
pub type LandResult = std::result::Result<crate::model::LandedBranch, String>;

pub async fn setup(paths: &Paths, workspace: String) -> Result<i32> {
    run_tracked(paths, workspace, vec![], Mode::Setup, None).await
}
