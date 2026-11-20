//! The one way every start settles its launch before sending it, whatever
//! space the daemon starts it in: what refuses comes before what asks, and
//! the mode before the pickers it decides. Only the daemon decides the
//! kind of space; this process reads what a mode says only to choose which
//! pickers the launch leaves open.
use anyhow::Result;
use shoal_protocol::launches::Launch;

use super::StartOptions;
use crate::cli::{args::create::StartArgs, commands::start::DaemonReads, context::Context};

mod arguments;
mod check;
mod located;
mod mode;
mod pickers;
mod repository;

pub(in crate::cli::commands) use arguments::check_start_arguments;
pub(in crate::cli::commands) use located::launch_args;
pub(in crate::cli::commands) use pickers::{check_model, pick_start_source};

/// Start what `args` ask for: refuse what they alone refuse, and what
/// their named mode refuses of the copy and fork pickers that making the
/// launch where the start is made opens, then [`settle`] it.
pub(in crate::cli::commands) async fn start(ctx: &Context, args: StartArgs) -> Result<i32> {
    arguments::check(&mut args.launch())?;
    arguments::ensure_nothing_dropped(&args)?;
    mode::ensure_located_fit(ctx, &args).await?;
    let launch = located::launch(ctx, &args).await?;
    settle(ctx, launch, &StartOptions::of(&args)).await
}

/// Settle `launch`, made where the start is made, and start it, placing
/// its agent as `options` ask: refuse what its arguments alone refuse,
/// find its repository and settings without asking, decide its mode where
/// nothing needs asking and check its agent, have the daemon refuse
/// what it would refuse, then pick on a terminal what the launch leaves
/// open, and send it.
pub(in crate::cli::commands) async fn settle(
    ctx: &Context,
    mut launch: Launch,
    options: &StartOptions,
) -> Result<i32> {
    arguments::check(&mut launch)?;
    let found = repository::found(ctx, &mut launch).await?;
    let settings = crate::launches::shape::launch_settings(&DaemonReads(ctx), &launch).await?;
    let choice = mode::decided(ctx, &launch, found.as_ref(), &settings).await?;
    let (mut found, mut settings) = match choice.clone().named() {
        Some(mode) => {
            mode::ensure_fits(&launch, &mode, &settings)?;
            repository::settled(ctx, &mut launch, &mode, (found, settings)).await?
        }
        None => (found, settings),
    };
    pickers::check_agent(&launch, &settings)?;
    let checked = check::check(ctx, &launch, options).await?;
    if let Some(workspace) = &checked {
        settings = check::workspace_settings(ctx, workspace).await?;
    }
    let mode = mode::chosen(ctx, choice, &launch, &settings)?;
    launch.mode = Some(mode.clone());
    mode::ensure_fits(&launch, &mode, &settings)?;
    (found, settings) = repository::settled(ctx, &mut launch, &mode, (found, settings)).await?;
    let worker = mode::starts_workspace(&launch, &mode, &settings)?;
    if worker && checked.is_none() {
        pickers::pick_source(ctx, &mut launch, found.as_ref()).await?;
    }
    pickers::pick_plan_issue(ctx, &mut launch, (found.as_ref(), &settings)).await?;
    pickers::pick_agent(ctx, &mut launch, &settings)?;
    let issue_linked = worker && check::issue_linked(checked.as_ref());
    super::place(ctx, launch, issue_linked, &settings, options).await
}
