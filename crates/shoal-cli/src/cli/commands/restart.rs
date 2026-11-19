//! Restarting agents, each in a new wrapper or in place, so each runs its
//! agent's current binary: the daemon restarts the agent of each space the
//! command asks it to, and says what it did. A chat's agent restarts in a
//! new place, as `shoal restore` restores a chat.
use anyhow::Result;
use futures_util::{StreamExt, stream::FuturesUnordered};

use crate::client::output::agents::Restarted;
use crate::{
    agent::Agent,
    cli::{args::SpaceTargets, context::Context},
    config::permissions::PermissionChange,
    model::{AgentRestart, RestartOutcome, Space},
};
use shoal_protocol::space_names::SpaceSelector;

mod cancel;
mod progress;
mod selection;

pub(super) use cancel::cancel;
use progress::Progress;

/// How many restarts the command asks for at once, each holding one of the
/// daemon's long-lived connections until the daemon answers.
const PARALLEL_RESTARTS: usize = 16;

pub(super) async fn run(
    ctx: &Context,
    targets: SpaceTargets,
    agent: Option<Agent>,
    restart: AgentRestart,
) -> Result<i32> {
    let selection = selection::load(ctx, targets, agent.as_ref()).await?;
    if crate::process::scoped::inherits_scope(&ctx.paths.state) {
        selection::ensure_below(&selection.spaces)?;
    }
    if selection.spaces.is_empty() && selection.skipped.is_empty() {
        let message = match &agent {
            Some(agent) => format!("No running {agent} agents"),
            None => "No running agents".into(),
        };
        ctx.emit(
            &message,
            Restarted {
                restarted: Vec::new(),
                pending: Vec::new(),
                failed: Vec::new(),
                skipped: Vec::new(),
                interrupted: false,
            },
        )?;
        return Ok(0);
    }
    if let Some(change) = &restart.change {
        ensure_change_fits(ctx, &selection.spaces, change).await?;
    }
    let total = selection.spaces.len() + selection.skipped.len();
    let mut progress = Progress::new(ctx, total, selection.places);
    for skipped in selection.skipped {
        progress.skipped(skipped);
    }
    let interrupted = tokio::select! {
        () = restart_all(ctx, selection.spaces, &restart, &mut progress) => false,
        signal = tokio::signal::ctrl_c() => {
            signal?;
            true
        }
    };
    if interrupted {
        // The daemon goes on with the restarts it was asked for.
        return progress.interrupted();
    }
    progress.finish()
}

/// Refuse `change` before any agent restarts where this scoped process
/// does not hold what it gives the agent of one of `spaces`.
async fn ensure_change_fits(
    ctx: &Context,
    spaces: &[Space],
    change: &PermissionChange,
) -> Result<()> {
    for space in spaces {
        let repository = space.repository_id.as_ref().map(Into::into);
        super::permissions::ensure_held(ctx, repository, &change.set).await?;
    }
    Ok(())
}

/// Ask the daemon to restart the agent of each of `spaces` as `restart`
/// says, up to [`PARALLEL_RESTARTS`] at once, telling `progress` what it
/// did with each.
async fn restart_all(
    ctx: &Context,
    spaces: Vec<Space>,
    restart: &AgentRestart,
    progress: &mut Progress<'_>,
) {
    let mut spaces = spaces.into_iter();
    let mut asked = FuturesUnordered::new();
    loop {
        while asked.len() < PARALLEL_RESTARTS
            && let Some(space) = spaces.next()
        {
            asked.push(async move {
                let outcome = restart_space(ctx, &space, restart).await;
                (space, outcome)
            });
        }
        let Some((space, outcome)) = asked.next().await else {
            return;
        };
        progress.ended(&space, outcome);
    }
}

/// What the daemon did with the restart of the agent of `space`.
async fn restart_space(
    ctx: &Context,
    space: &Space,
    restart: &AgentRestart,
) -> Result<RestartOutcome> {
    let selector = SpaceSelector::id(&space.id);
    match crate::client::agents::restart(&ctx.paths, selector, restart.clone()).await {
        Err(error) => Err(with_restore(ctx, space, error).await),
        outcome => outcome,
    }
}

/// `error`, of the failed restart of `space`'s agent, naming how to restore
/// the agent when the space saved stopped work for that. The daemon's error
/// stays its cause, which callers can inspect.
async fn with_restore(ctx: &Context, space: &Space, error: anyhow::Error) -> anyhow::Error {
    let saved = crate::client::saved::read(&ctx.paths, SpaceSelector::id(&space.id)).await;
    match saved.is_ok_and(|saved| saved.is_restorable()) {
        true => error.context(format!(
            "the agent stays stopped; restore it with shoal restore {}",
            space.number
        )),
        false => error,
    }
}
