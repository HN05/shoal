//! `shoal restart`.
use anyhow::Result;
use futures_util::{StreamExt, stream::FuturesUnordered};

use crate::client::output::agents::Restarted;
use crate::{
    agent::Agent,
    cli::{args::SpaceTargets, context::Context, ui::Acting},
    config::permissions::PermissionChange,
    model::{AgentRestart, RestartOutcome},
};
use shoal_protocol::space_names::SpaceSelector;
use target::Target;

mod cancel;
mod progress;
mod selection;
mod target;

pub(super) use cancel::{cancel, cancelling};
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
    let restarting = restarting();
    let selection = selection::load(ctx, targets, agent.as_ref(), &restarting);
    let selection = selection.await?;
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

/// Another machine checks the permissions given to its own spaces.
async fn ensure_change_fits(
    ctx: &Context,
    spaces: &[Target],
    change: &PermissionChange,
) -> Result<()> {
    for space in spaces.iter().filter(|space| space.machine.is_none()) {
        let repository = space.repository.as_ref().map(Into::into);
        super::permissions::ensure_held(ctx, repository, &change.set).await?;
    }
    Ok(())
}

async fn restart_all(
    ctx: &Context,
    spaces: Vec<Target>,
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

async fn restart_space(
    ctx: &Context,
    space: &Target,
    restart: &AgentRestart,
) -> Result<RestartOutcome> {
    let selector = space.selector();
    match (crate::client::agents::restart(selector, restart.clone()))
        .send(&ctx.paths)
        .await
    {
        Err(error) => Err(with_restore(ctx, space, error).await),
        outcome => outcome,
    }
}

/// The daemon's error stays the cause, which callers can inspect.
async fn with_restore(ctx: &Context, space: &Target, error: anyhow::Error) -> anyhow::Error {
    if space.machine.is_some() {
        return error;
    }
    let saved = crate::client::saved::read(&ctx.paths, SpaceSelector::id(&space.id)).await;
    match saved.is_ok_and(|saved| saved.is_restorable()) {
        true => error.context(format!(
            "the agent stays stopped; resume it with shoal resume {}",
            space.number
        )),
        false => error,
    }
}

/// How the agents restart decides nothing of which spaces it reaches.
pub(super) fn restarting() -> Acting {
    Acting::request(|space| crate::client::agents::restart(space, AgentRestart::default()))
}
