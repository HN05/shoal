//! Following a workspace's preparation, the worktree, setup and post-setup
//! hook the daemon runs for it, and settling a setup that failed. What the
//! process does its own way, showing the steps and asking a user what to do
//! with a failed setup, is its [`Frontend`]'s; a process without a user
//! takes [`Unattended`], whose errors name the commands that settle it.
mod setup;

use anyhow::{Result, anyhow};

pub use setup::{
    SetUp, follow_setup, ignore_failure, run_post_setup, settle_setup, setup_workspace,
};

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

/// What a process that prepares a workspace does its own way.
pub trait Frontend {
    /// Whether the process prints JSON, which keeps stdout for its result.
    fn json(&self) -> bool;

    /// What shows an operation's steps while the process follows it.
    fn display(&self) -> impl Display;

    /// What to do with `workspace`, whose setup failed with `error`, or did
    /// not start unless `ran`; `None` where no user decides.
    fn settle(&self, workspace: &Workspace, error: &str, ran: bool) -> Result<Option<Settling>>;

    /// Tell a user `message`, that setup's processes may still run, which
    /// fails where no user reads it.
    fn unverified(&self, message: String) -> Result<()>;
}

/// What a user decided to do with a workspace whose setup failed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Settling {
    /// Run setup again.
    Retry,
    /// Delete the workspace with its setup's changes.
    Delete,
    /// Accept the workspace as setup left it.
    Ignore,
    /// Keep the workspace as it is and stop.
    Cancel,
}

/// The frontend of a process without a user: it shows nothing and leaves
/// a failed setup to the commands its error names.
pub struct Unattended {
    pub json: bool,
}

impl Frontend for Unattended {
    fn json(&self) -> bool {
        self.json
    }

    fn display(&self) -> impl Display {
        Quiet
    }

    fn settle(&self, _: &Workspace, _: &str, _: bool) -> Result<Option<Settling>> {
        Ok(None)
    }

    fn unverified(&self, message: String) -> Result<()> {
        Err(anyhow!(message))
    }
}

/// Follow `preparing` to its end below `frontend`'s display, after what
/// assigning its issue on the forge warned of, returning the prepared
/// workspace or how preparing it failed.
pub async fn follow(
    paths: &CommandPaths,
    frontend: &impl Frontend,
    mut preparing: Preparing<'_, CommandPaths>,
) -> Result<Result<Prepared, Unprepared>> {
    for warning in preparing.warnings() {
        eprintln!("warning: {warning}");
    }
    let operation = preparing.operation().clone();
    let shown = Shown {
        operation: &operation,
        lines: Lines::Command,
        json: frontend.json(),
    };
    let mut display = frontend.display();
    following::show(paths, shown, &mut display, async || preparing.next().await).await
}

/// The workspace `preparing` sets up, once setup and its post-setup hook
/// succeeded, for a process without a user; a failure is its error.
pub async fn set_up(
    paths: &CommandPaths,
    json: bool,
    preparing: Preparing<'_, CommandPaths>,
) -> Result<Workspace> {
    let frontend = Unattended { json };
    match follow_setup(paths, &frontend, preparing).await? {
        Some(set_up) => set_up.finish(paths, &frontend).await,
        None => Err(anyhow!("setup of the workspace did not finish")),
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
