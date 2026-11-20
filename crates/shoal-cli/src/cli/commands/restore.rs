//! `shoal restore`: reopening removed spaces, whose agents resume as
//! `shoal resume` resumes them, returning workspaces to their checkpoints,
//! and discarding a space's stopped agent.
use anyhow::{Result, ensure};

use super::resume;
use crate::cli::{args::SpaceTargets, context::Context};
use crate::client::output::{restore::Reopened, resume::Queued};
use crate::model::RemovedSpace;
use shoal_ids::{checkpoint_numbers::CheckpointNumber, execution_ids::ExecutionId};
use shoal_protocol::space_names::SpaceSelector;

/// What `shoal restore` does with the spaces it names.
pub(super) enum Action {
    /// Return the workspace to this checkpoint, or to one picked.
    Checkpoint(Option<CheckpointNumber>),
    /// Discard the agents of the spaces named, every space's for `all`,
    /// only `execution`'s when given.
    DiscardAgent {
        all: bool,
        execution: Option<ExecutionId>,
    },
    /// Reopen the removed spaces named, or picked, resuming their agents as
    /// `resume` says, or leaving them saved without it.
    Reopen { resume: Option<resume::Options> },
}

/// `shoal restore`: do `action` with the `spaces` named.
pub(super) async fn run(ctx: &Context, spaces: Vec<SpaceSelector>, action: Action) -> Result<i32> {
    match action {
        Action::Checkpoint(checkpoint) => checkpoints::run(ctx, spaces, checkpoint).await,
        Action::DiscardAgent { all, execution } => {
            let targets = SpaceTargets { space: spaces, all };
            discard::run(ctx, targets, execution).await
        }
        Action::Reopen { resume } => match targets::removed(ctx, spaces).await? {
            Some(removed) => reopen(ctx, removed, resume).await,
            None => Ok(0),
        },
    }
}

/// Open the `removed` spaces again, then resume the agents their removal
/// saved as `resume` says, or report them reopened with their agents saved
/// without it; an error once one opened names those opened before it.
async fn reopen(
    ctx: &Context,
    removed: Vec<RemovedSpace>,
    resume: Option<resume::Options>,
) -> Result<i32> {
    if crate::process::scoped::inherits_scope(&ctx.paths.state) {
        resume::ensure_queued()?;
    }
    if resume.as_ref().is_some_and(|options| options.focus) {
        ensure_one_agent(&removed)?;
    }
    let reopened = reopen::reopen_each(ctx, &removed).await?;
    let reopening = reopen::Reopening::of(&reopened);
    let resumed = match resume {
        Some(options) => resume::reopened(ctx, reopened, options).await,
        None => report_reopened(ctx, reopened),
    };
    resumed.map_err(|error| reopening.explain(error))
}

/// Report the spaces `reopened` with their agents left saved.
fn report_reopened(ctx: &Context, reopened: Vec<Reopened>) -> Result<i32> {
    let reopened = Queued {
        reopened,
        ..Queued::default()
    };
    resume::reports::queued(ctx, &reopened)?;
    Ok(0)
}

/// Refuse `--focus`, which resumes one agent in this terminal, unless one of
/// the `removed` spaces saved agents, before any opens again.
fn ensure_one_agent(removed: &[RemovedSpace]) -> Result<()> {
    let with_agents = (removed.iter())
        .filter(|space| !(space.agents.is_empty() && space.last_sessions.is_empty()))
        .count();
    ensure!(with_agents == 1, "--focus resumes the agent of one space");
    Ok(())
}

mod checkpoints;
mod discard;
pub(super) mod import;
pub(super) mod reopen;
mod targets;

pub(super) use checkpoints::checkpoint;
