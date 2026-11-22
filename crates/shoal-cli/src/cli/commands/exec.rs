//! `shoal run` with a command: running it in a workspace.
use std::ffi::OsString;

use anyhow::Result;

use crate::cli::{
    context::Context,
    ui::{self, Acting, Fallback},
};
use shoal_protocol::space_names::SpaceSelector;

pub(in crate::cli::commands) async fn exec(
    ctx: &Context,
    workspace: Option<SpaceSelector>,
    command: Vec<OsString>,
) -> Result<i32> {
    let workspace =
        ui::select_workspace(ctx, workspace, Fallback::CurrentDirectory(&Acting::here())).await?;
    shoal_workers::execution::run_command(&ctx.paths, workspace, command).await
}
