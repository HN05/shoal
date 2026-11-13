//! Where a restore runs a stopped agent's restore outside this terminal: a
//! workspace's in a place its terminal host opens beside or where the agent
//! stopped, a chat's in a background place, as its start opens one.
use std::ffi::OsString;

use anyhow::Result;

use crate::client::space_names::SpaceSelector;
use crate::{
    agents::HomeRecords,
    cli::{context::Context, terminal},
    config::Effective,
    model::PresentSpace,
    sessions::Origin,
};

/// Run Shoal with `args`, a restore of the agent of `space`, in the
/// background, returning what such starts run in, or `None` when this
/// terminal cannot start them.
pub(super) async fn run_in_background(
    ctx: &Context,
    settings: &Effective,
    space: &PresentSpace,
    args: &[OsString],
) -> Result<Option<&'static str>> {
    match space {
        PresentSpace::Workspace(workspace) => {
            terminal::run_in_background(ctx, settings, workspace, args).await
        }
        PresentSpace::Chat(_) => {
            start_chat(ctx, settings, space, args).await?;
            Ok(Some("a background place"))
        }
    }
}

/// Run Shoal with `args`, a restore of the agent of `space` that stopped in
/// `stopped`, for the daemon's worker, which has no terminal: where the
/// agent stopped when that place is back at its prompt, otherwise in a
/// background place.
pub(super) async fn restore_queued(
    ctx: &Context,
    settings: &Effective,
    space: &PresentSpace,
    stopped: Option<&Origin>,
    args: &[OsString],
) -> Result<()> {
    match space {
        PresentSpace::Workspace(workspace) => {
            terminal::restore_queued(ctx, settings, workspace, stopped, args).await
        }
        PresentSpace::Chat(_) => {
            if terminal::rerun_where_stopped(ctx, settings, stopped, args)
                .await?
                .is_some()
            {
                return Ok(());
            }
            start_chat(ctx, settings, space, args).await
        }
    }
}

/// Run Shoal with `args` for the chat `space` in a new background place of
/// the first host that would start a queued agent, in its home.
async fn start_chat(
    ctx: &Context,
    settings: &Effective,
    space: &PresentSpace,
    args: &[OsString],
) -> Result<()> {
    let records = HomeRecords::read(&ctx.paths, &SpaceSelector::id(space.id())).await?;
    let home = records.home()?;
    let repository = match &records {
        HomeRecords::Chat { repository, .. } => repository.as_ref(),
        HomeRecords::Workspace(_) => None,
    };
    let name = space.name().as_str();
    let start = terminal::start_chat_in_background(
        ctx,
        settings,
        repository,
        home.path(),
        name,
        args,
        true,
    );
    start.await?;
    Ok(())
}
