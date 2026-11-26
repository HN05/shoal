//! Sending a start whose agent runs in its caller, and the place worker that
//! carries out its plan; [`shoal_workers::starts`] runs them.
use anyhow::Result;
use shoal_workers::starts::{self, Placed};

use super::placement::{self, Attach};
use crate::{
    cli::{commands::links, commands::preparation, context::Context},
    config::Effective,
};
use shoal_protocol::launches::Launch;
use shoal_protocol::protocol::{Launched, Started};

/// An `issue_linked` workspace's own issue prompts the agent where the launch
/// names nothing to start on.
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

/// A workspace's agent runs here once its preparation ends, unless it waits for
/// other work or disk space, which leaves the agent to the queue.
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

/// Text reports the machine the hub placed the start on first. JSON puts it
/// inside a chat's launch, or after a workspace's documents, so a start's first
/// document stays its space.
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
