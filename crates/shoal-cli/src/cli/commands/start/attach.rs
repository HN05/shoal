//! Sending a start whose agent runs in its caller, here or in a place it
//! opens beside this terminal, and the place worker that carries out such a
//! start's plan there; [`shoal_workers::starts`] runs them.
use anyhow::Result;
use shoal_workers::starts::{self, Placed};

use super::placement::{self, Attach};
use crate::{
    cli::{commands::links, commands::preparation, context::Context},
    config::Effective,
};
use shoal_protocol::launches::Launch;
use shoal_protocol::protocol::{Launched, Started};

/// Send `launch` under `settings`, its agent, the one it names or else
/// `agent.default`, run in this caller where `options` and this terminal
/// say, else by the daemon, which for a start on another machine this
/// terminal attaches to where `options` show it here. An `issue_linked`
/// workspace's own issue prompts the agent where the launch names nothing
/// to start on.
pub(in crate::cli::commands) async fn place(
    ctx: &Context,
    mut launch: Launch,
    issue_linked: bool,
    settings: &Effective,
    options: &super::StartOptions,
) -> Result<i32> {
    if super::shown::shown_elsewhere(&launch, options) {
        return super::shown::start(ctx, launch).await;
    }
    let agent = (launch.agent.clone()).or_else(|| settings.agent.default.clone());
    let started = (&launch, issue_linked);
    let attach = placement::attach(ctx, started, agent.as_ref(), settings, options);
    let placed = (attach, agent.as_ref());
    super::place_agent(ctx, (&mut launch, issue_linked), placed, settings, options)?;
    send(
        ctx,
        launch,
        (options.wait, agent),
        attach.zip(Some(settings)),
    )
    .await
}

/// Send `launch`, which `agent` runs if it names one, and report what the
/// daemon started, or run its agent in this caller as `attach` says under
/// the start's settings: a workspace's once its preparation ends, unless
/// it waits for other work or disk space, which leaves the agent to the
/// queue's start. A start that runs nothing here reports a new workspace
/// once it is recorded, or with `wait` once its preparation ends.
pub(in crate::cli::commands) async fn send(
    ctx: &Context,
    launch: Launch,
    (wait, agent): (bool, Option<crate::agent::Agent>),
    attach: Option<(Attach, &Effective)>,
) -> Result<i32> {
    let launched = crate::client::launches::launch(&ctx.paths, launch).await?;
    let Some(attach) = attach else {
        return report_detached(ctx, launched, (wait, agent)).await;
    };
    match &launched.start {
        Started::Detached(_) => super::reports::report_started(ctx, launched),
        // A preparation that waits for other work or disk space leaves the
        // agent to the queue's start once it is done.
        Started::Preparing {
            operation: None, ..
        } => preparation::report_launched(ctx, launched, (false, agent)).await,
        _ => {
            links::print_warnings(&launched.warnings);
            starts::run(ctx, Placed::of(launched), attach).await
        }
    }
}

/// Report what the daemon `launched` for a start whose agent, which
/// `agent` runs if it names one, runs in no terminal of this caller, as
/// [`send`] does, with the machine the hub placed it on: first in text, and
/// in JSON inside a chat's launch, or after a workspace's documents, so a
/// start's first document stays its space.
async fn report_detached(
    ctx: &Context,
    launched: Launched,
    (wait, agent): (bool, Option<crate::agent::Agent>),
) -> Result<i32> {
    let placement = launched.placement.clone();
    if let Some(decision) = placement.as_ref().filter(|_| !ctx.json) {
        super::reports::report_placement(ctx, decision)?;
    }
    if let Started::Queued { .. } | Started::Detached(_) = &launched.start {
        return super::reports::report_started(ctx, launched);
    }
    let code = preparation::report_launched(ctx, launched, (wait, agent)).await?;
    if let Some(decision) = placement.as_ref().filter(|_| ctx.json) {
        super::reports::report_placement(ctx, decision)?;
    }
    Ok(code)
}
