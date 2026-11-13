//! Where a restore runs a stopped agent's restore outside this terminal: a
//! workspace's in a place its terminal host opens beside it, a chat's in a
//! background place, as its start opens one.
use std::ffi::OsString;

use anyhow::Result;

use crate::{
    cli::{context::Context, terminal},
    config::Effective,
    model::PresentSpace,
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
            let paths = &ctx.paths;
            crate::terminal::queued::restore_chat(paths, settings, space, args).await?;
            Ok(Some("a background place"))
        }
    }
}
