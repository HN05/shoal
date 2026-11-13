//! Restarting agents, each in a new wrapper or in place, together or one at
//! a time, so each runs its agent's current binary. A chat's agent restarts
//! in a new place, as `shoal restore` restores a chat.
use anyhow::{Result, ensure};

use crate::client::output::agents::Restarted;
use crate::client::restarts::{self, Options, Progress as _, Restart, running_agents};
use crate::{
    agent::Agent,
    cli::{args::SpaceTargets, context::Context},
    config::permissions::PermissionChange,
};

mod progress;
mod selection;

use progress::Progress;

pub(super) async fn run(
    ctx: &Context,
    targets: SpaceTargets,
    agent: Option<Agent>,
    options: Options,
) -> Result<i32> {
    let scoped = crate::env::inherits_scope(&ctx.paths.state);
    if scoped {
        ensure_named(&targets)?;
    }
    let selection = selection::load(ctx, targets).await?;
    if scoped {
        selection::ensure_below(&selection)?;
    }
    let plan = selection.plan(agent.as_ref());
    if plan.restarts.is_empty() && plan.skipped.is_empty() {
        let message = match &agent {
            Some(agent) => format!("No running {agent} agents"),
            None => "No running agents".into(),
        };
        ctx.emit(
            &message,
            Restarted {
                restarted: Vec::new(),
                failed: Vec::new(),
                skipped: Vec::new(),
                interrupted: false,
                pending: Vec::new(),
            },
        )?;
        return Ok(0);
    }
    if let Some(change) = &options.change {
        ensure_change_fits(ctx, &plan.restarts, change).await?;
    }
    let mut progress = Progress::new(ctx, &plan.restarts, plan.skipped.len());
    for skipped in plan.skipped {
        progress.skipped(skipped);
    }
    let interrupted = tokio::select! {
        restarted = restarts::restart_all(&ctx.paths, plan.restarts, &plan.run, &options, &mut progress) => {
            restarted?;
            false
        }
        signal = tokio::signal::ctrl_c() => {
            signal?;
            true
        }
    };
    if interrupted {
        // The daemon goes on with the restarts it was asked for.
        let agents = running_agents(&ctx.paths).await?;
        return progress.interrupted(&agents, agent.as_ref());
    }
    progress.finish()
}

/// Refuse a restart in a scoped process, a workspace's or a chat's agent,
/// that does not name the spaces it restarts. The daemon refuses each space
/// not below the caller's, which needs the scope token: a process that
/// cleared it stays refused.
fn ensure_named(targets: &SpaceTargets) -> Result<()> {
    ensure!(
        crate::env::is_scoped() && !targets.all && !targets.space.is_empty(),
        "scoped processes restart only the spaces below their own, by name: shoal restart <space>"
    );
    Ok(())
}

/// Refuse `change` before any agent restarts where this scoped process
/// does not hold what it gives an agent `restarts` restarts, or where one
/// joins a restart in progress, which keeps the permissions it started with.
async fn ensure_change_fits(
    ctx: &Context,
    restarts: &[Restart],
    change: &PermissionChange,
) -> Result<()> {
    for restart in restarts {
        ensure!(
            restart.joins.is_none(),
            "{} restarts already; restart it with --permission once it runs",
            restart.space.name()
        );
        let repository = restart.space.repository_id().map(Into::into);
        super::permissions::ensure_held(ctx, repository, &change.set).await?;
    }
    Ok(())
}
