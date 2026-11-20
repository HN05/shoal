//! Resuming one space's stopped agent at once, whatever the agent limits,
//! as `shoal resume --now` does for one space: from the caller beside its
//! terminal, or through the queue for a caller without one.
use anyhow::Result;
use shoal_ids::execution_ids::ExecutionId;

use super::{
    Resuming, queue,
    sessions::{self, Starts},
    space::{self, Choice},
};
use crate::{
    client::{
        connection::CommandPaths,
        output::resume::{Queued, StartedRestores, StoppedCommands},
    },
    config::permissions::PermissionChange,
    places::Launcher,
};
use shoal_protocol::space_names::SpaceSelector;

/// What starts a resumed agent.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Starter {
    /// The caller, whose terminal a user is on: beside it, as the
    /// `Launcher` starts work, or else in that terminal.
    Caller(Launcher),
    /// The daemon, through the queue, for a caller without a terminal.
    Daemon,
}

/// What resuming a space's agent at once did.
pub enum Resumed {
    /// Started it, or left it to the caller's terminal.
    Starts(Starts),
    /// Queued its restore, which the daemon starts at once.
    Queued(Queued),
    /// Found only stopped commands, which it reported to the user.
    Commands(StoppedCommands),
}

/// Resume the stopped agent or last session of the space `space` names at
/// once, the `execution` one when given, as `starter` starts it, making
/// `change` when given; `resuming` picks among several and tells the user
/// what no agent receives and what fails.
pub async fn now(
    paths: &CommandPaths,
    starter: Starter,
    space: &SpaceSelector,
    execution: Option<&ExecutionId>,
    change: Option<&PermissionChange>,
    resuming: &impl Resuming,
) -> Result<Resumed> {
    match starter {
        Starter::Caller(launcher) => {
            let choice = space::choose(paths, space, execution, change, resuming).await?;
            start(paths, launcher, choice, resuming).await
        }
        Starter::Daemon => {
            let queued = queue_now(paths, space, execution, change, resuming).await?;
            Ok(Resumed::Queued(queued))
        }
    }
}

/// Start the agent `choice` restores beside the caller's terminal, as
/// `launcher` starts work, or leave it to that terminal.
async fn start(
    paths: &CommandPaths,
    launcher: Launcher,
    choice: Choice,
    resuming: &impl Resuming,
) -> Result<Resumed> {
    let restore = match choice {
        Choice::Commands(commands) => return Ok(Resumed::Commands(commands)),
        Choice::Queued(queued) => return Ok(Resumed::Queued(queued)),
        Choice::Restore(restore) => *restore,
    };
    let sessions = vec![(restore.space, restore.picked)];
    let output = StartedRestores::default();
    let starts = sessions::start(paths, launcher, sessions, true, output, resuming).await?;
    Ok(Resumed::Starts(starts))
}

/// Queue the restores of the stopped agents and last sessions of the space
/// `space` names, only `execution`'s when given, for the daemon to start at
/// once, making `change` when given.
async fn queue_now(
    paths: &CommandPaths,
    space: &SpaceSelector,
    execution: Option<&ExecutionId>,
    change: Option<&PermissionChange>,
    resuming: &impl Resuming,
) -> Result<Queued> {
    let space = space::present_space(paths, space).await?;
    let saved = space::saved(paths, &space, execution).await?;
    let stopped = vec![(space, saved)];
    queue::ensure_daemon_starts(paths, &stopped).await?;
    super::permissions::apply(paths, &stopped, &[], change).await?;
    Ok(queue::queue_each(paths, stopped, true, resuming).await)
}
