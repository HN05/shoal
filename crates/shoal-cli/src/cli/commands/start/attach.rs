//! Sending a start whose agent runs in its caller, and the place worker that
//! carries out its plan; [`shoal_workers::starts`] runs them.
use anyhow::Result;
use shoal_workers::starts::{self, Placed};

use super::placement::{self, Attach};
use crate::{
    cli::{commands::links, commands::preparation, context::Context},
    config::Effective,
};
use shoal_protocol::hub::placement::Decision;
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

/// The agent runs in this caller as `attach` says, unless the hub placed the
/// start on another machine, which runs it there.
pub(in crate::cli::commands) async fn send(
    ctx: &Context,
    launch: Launch,
    (wait, agent): (bool, Option<crate::agent::Agent>),
    attach: Option<(Attach, &Effective)>,
) -> Result<i32> {
    let mut launched = crate::client::launches::launch(&ctx.paths, launch).await?;
    let Some(attach) = attach else {
        return report_detached(ctx, launched, (wait, agent)).await;
    };
    if launched.machine.is_some() {
        return attach_elsewhere(ctx, launched, (wait, agent)).await;
    }
    let placement = launched.placement.take();
    with_placement(ctx, placement, run_here(ctx, launched, attach, agent)).await
}

/// A workspace's agent runs here once its preparation ends, unless it waits for
/// other work or disk space, which leaves the agent to the queue.
async fn run_here(
    ctx: &Context,
    launched: Launched,
    attach: (Attach, &Effective),
    agent: Option<crate::agent::Agent>,
) -> Result<i32> {
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

/// A terminal attaches to the agent the other machine started detached, as
/// `--machine` with `--here` does. Another caller leaves it running there.
async fn attach_elsewhere(
    ctx: &Context,
    mut launched: Launched,
    (wait, agent): (bool, Option<crate::agent::Agent>),
) -> Result<i32> {
    if !ctx.on_terminal() {
        return report_detached(ctx, launched, (wait, agent)).await;
    }
    let placement = launched.placement.take();
    with_placement(ctx, placement, super::shown::attach_launched(ctx, launched)).await
}

/// JSON puts the placement inside a chat's launch.
async fn report_detached(
    ctx: &Context,
    launched: Launched,
    (wait, agent): (bool, Option<crate::agent::Agent>),
) -> Result<i32> {
    if let Started::Queued { .. } | Started::Detached(_) = &launched.start {
        let placement = launched.placement.clone().filter(|_| !ctx.json);
        let started = async { super::reports::report_started(ctx, launched) };
        return with_placement(ctx, placement, started).await;
    }
    let placement = launched.placement.clone();
    let reported = preparation::report_launched(ctx, launched, (wait, agent));
    with_placement(ctx, placement, reported).await
}

/// Text reports the machine the hub placed the start on before `start`, which
/// may give the agent the terminal. JSON reports it after, so a start's first
/// document stays its space.
async fn with_placement(
    ctx: &Context,
    placement: Option<Decision>,
    start: impl Future<Output = Result<i32>>,
) -> Result<i32> {
    if let Some(decision) = placement.as_ref().filter(|_| !ctx.json) {
        super::reports::report_placement(ctx, decision)?;
    }
    let code = start.await?;
    if let Some(decision) = placement.as_ref().filter(|_| ctx.json) {
        super::reports::report_placement(ctx, decision)?;
    }
    Ok(code)
}
