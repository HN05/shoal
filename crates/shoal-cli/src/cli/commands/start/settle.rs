//! The one way every start settles its launch before sending it, whatever
//! space the daemon starts it in: the issue or PR it asks for, whose labels
//! may decide the mode, then what refuses before what asks, and the mode
//! before the pickers it decides. Only the daemon decides the
//! kind of space; this process reads what a mode says only to choose which
//! pickers the launch leaves open.
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
/// find its repository and settings without asking, pick the issue or PR
/// it asks for without naming, decide its mode where
/// nothing needs asking and check its agent, have the daemon refuse
/// what it would refuse, asking on a terminal about a stopped agent the
/// launch does not replace, then pick on a terminal what the launch leaves
/// open, and send it.
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
    let choice = mode::decided(ctx, &launch, found.as_ref(), (&settings, &modes)).await?;
    let (mut found, mut settings) = match choice.clone().named() {
        Some(mode) => {
            mode::ensure_fits(&launch, &mode, &modes)?;
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

/// The repository and settings `launch` goes on with once a terminal has
/// picked the issue and PR it asks for without naming them, from the
/// `found` repository with its `settings`, else a picked one: first what
/// its named mode and its agent refuse, so no picker asks for what would
/// be refused.
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
