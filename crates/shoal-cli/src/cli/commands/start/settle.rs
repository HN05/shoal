//! The one way every start settles its launch before sending it. Only the
//! daemon decides the kind of space; this process reads what a mode says only
//! to choose which pickers the launch leaves open.
use anyhow::Result;
use shoal_protocol::launches::Launch;

use super::StartOptions;
use crate::{
    cli::{
        args::create::StartArgs,
        commands::start::{launch_settings, start_modes},
        context::Context,
    },
    config::Effective,
    model::Repository,
};

mod arguments;
mod check;
mod located;
mod mode;
mod pickers;
mod repository;
mod stopped;

pub(in crate::cli::commands) use arguments::check_start_arguments;
pub(in crate::cli::commands) use located::{launch_args, starting};
pub(in crate::cli::commands) use pickers::{check_model, pick_start_source};

/// Refuse what `args` alone refuse, then [`settle`] the launch.
pub(in crate::cli::commands) async fn start(ctx: &Context, args: StartArgs) -> Result<i32> {
    arguments::check(&mut args.launch())?;
    arguments::ensure_nothing_dropped(&args)?;
    mode::ensure_located_fit(ctx, &args).await?;
    let launch = located::launch(ctx, &args).await?;
    settle(ctx, launch, &StartOptions::of(&args)).await
}

/// The order matters: what refuses goes before what asks, and the mode before
/// the pickers it decides.
pub(in crate::cli::commands) async fn settle(
    ctx: &Context,
    mut launch: Launch,
    options: &StartOptions,
) -> Result<i32> {
    arguments::check(&mut launch)?;
    repository::take_parents(ctx, &launch).await?;
    let found = repository::found(ctx, &mut launch).await?;
    let settings = launch_settings(ctx, &launch).await?;
    let (found, settings) = picked_items(ctx, &mut launch, (found, settings)).await?;
    let modes = start_modes(ctx, &launch, &settings).await?;
    let choice = mode::decided(ctx, &launch, &modes).await?;
    let (mut found, mut settings) = match choice.clone().named() {
        Some(mode) => {
            mode::ensure_fits(&launch, &mode, &modes)?;
            // The daemon's checks then read no labels again.
            launch.mode = Some(mode.clone());
            let mode = (&mode, &modes);
            repository::settled(ctx, &mut launch, mode, (found, settings)).await?
        }
        None => repository::required(ctx, &mut launch, (found, settings)).await?,
    };
    pickers::check_agent(&launch, &settings)?;
    let kind_checked = crate::launches::decides_kind(&launch);
    let checked = match stopped::checked(ctx, &mut launch, options).await? {
        stopped::Checked::Goes(workspace) => workspace.map(|workspace| *workspace),
        stopped::Checked::Resumed(code) => return Ok(code),
    };
    if let Some(workspace) = &checked {
        settings = check::workspace_settings(ctx, workspace).await?;
    }
    let modes = start_modes(ctx, &launch, &settings).await?;
    let mode = mode::chosen(ctx, choice, &launch, &modes)?;
    launch.mode = Some(mode.clone());
    mode::ensure_fits(&launch, &mode, &modes)?;
    let settled = (&mode, &modes);
    (found, settings) = repository::settled(ctx, &mut launch, settled, (found, settings)).await?;
    let worker = mode::starts_workspace(&launch, &mode, &modes)?;
    let picked = worker && checked.is_none() && {
        pickers::pick_source(ctx, &mut launch, found.as_ref()).await?
    };
    // Where the first check left the kind open or a picker named the
    // source, a terminal asks about the stopped agent of the workspace the
    // start goes into once that is known.
    if worker
        && (picked || !kind_checked)
        && let Some(code) = stopped::resumed(ctx, &mut launch, options).await?
    {
        return Ok(code);
    }
    pickers::pick_agent(ctx, &mut launch, &settings)?;
    let issue_linked = worker && check::issue_linked(checked.as_ref());
    super::place(ctx, launch, issue_linked, &settings, options).await
}

/// What the named mode and agent refuse comes first, so no picker asks for what
/// would be refused.
async fn picked_items(
    ctx: &Context,
    launch: &mut Launch,
    (found, settings): (Option<Repository>, Effective),
) -> Result<(Option<Repository>, Effective)> {
    if !pickers::picks_items(launch) {
        return Ok((found, settings));
    }
    if let Some(mode) = launch.mode.clone() {
        let modes = start_modes(ctx, launch, &settings).await?;
        mode::ensure_fits(launch, &mode, &modes)?;
    }
    let (found, settings) = repository::required(ctx, launch, (found, settings)).await?;
    pickers::check_agent(launch, &settings)?;
    pickers::pick_items(ctx, launch, (found.as_ref(), &settings)).await?;
    Ok((found, settings))
}
