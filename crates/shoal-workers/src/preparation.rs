//! Following a workspace's preparation, the worktree, setup and post-setup
//! hook the daemon runs for it, and the errors a failed setup reports to a
//! process without a user to ask what to do with the workspace.
use anyhow::{Result, anyhow};

use crate::{
    client::{
        connection::CommandPaths,
        preparation::{Preparing, Unprepared},
    },
    following::{self, Display, Lines, Quiet, Shown},
    model::Workspace,
    operations::error,
    protocol::operations::{Failure, Prepared},
};

/// Follow `preparing` to its end below `display`, after what assigning its
/// issue on the forge warned of, returning the prepared workspace or how
/// preparing it failed. `json` says whether the process prints JSON.
pub async fn follow(
    paths: &CommandPaths,
    json: bool,
    display: &mut impl Display,
    mut preparing: Preparing<'_, CommandPaths>,
) -> Result<Result<Prepared, Unprepared>> {
    for warning in preparing.warnings() {
        eprintln!("warning: {warning}");
    }
    let operation = preparing.operation().clone();
    let shown = Shown {
        operation: &operation,
        lines: Lines::Command,
        json,
    };
    following::show(paths, shown, display, async || preparing.next().await).await
}

/// The workspace `preparing` sets up, once setup and its post-setup hook
/// succeeded, for a process without a user; a failure is its error.
pub async fn set_up(
    paths: &CommandPaths,
    json: bool,
    preparing: Preparing<'_, CommandPaths>,
) -> Result<Workspace> {
    match follow(paths, json, &mut Quiet, preparing).await? {
        Ok(prepared) => Ok(*prepared.workspace),
        Err(unprepared) => Err(unattended(unprepared)),
    }
}

/// The error of the preparation that failed as `unprepared` says, where
/// no user decides what to do with the workspace.
fn unattended(unprepared: Unprepared) -> anyhow::Error {
    let set_up = unprepared.set_up();
    let Unprepared { workspace, failure } = unprepared;
    match failure {
        Failure::SetupUnverified { exit_code } => anyhow!(unverified(&workspace, exit_code)),
        failure @ (Failure::WorktreeFailed { .. } | Failure::HookFailed { .. }) => error(failure),
        failure if set_up => error(failure),
        failure => unsettled(&workspace, failure),
    }
}

/// What a setup command that exited with `code` leaves when Shoal cannot
/// tell whether its processes ended: the workspace is kept for the user to
/// stop them, then retry or skip setup.
pub fn unverified(workspace: &Workspace, code: i32) -> String {
    let (name, number) = (&workspace.name, workspace.number);
    let reason = workspace
        .error
        .as_deref()
        .unwrap_or("process state is unknown");
    format!(
        "Setup verification incomplete for {name} (command exit {code}): {reason}\nWorkspace retained at {}. Stop its processes with `shoal stop {number}`, then retry setup with `shoal setup {number}` or skip it with `shoal setup {number} --skip`.",
        workspace.path.display()
    )
}

/// The error of a setup that failed with `failure`, or did not start,
/// naming what retries, skips or deletes the workspace it kept.
pub fn unsettled(workspace: &Workspace, failure: Failure) -> anyhow::Error {
    let ran = !matches!(failure, Failure::SetupNotStarted { .. });
    let error = format!("{:#}", error(failure));
    let (name, number) = (&workspace.name, workspace.number);
    match ran {
        true => anyhow!(
            "setup failed for {name}: {error}; workspace retained. Retry with `shoal setup {number}`, skip it with `shoal setup {number} --skip`, or delete with `shoal rm {number} --yes`"
        ),
        false => anyhow!(
            "setup of {name} did not start: {error}; workspace retained. Retry with `shoal setup {number}`"
        ),
    }
}
