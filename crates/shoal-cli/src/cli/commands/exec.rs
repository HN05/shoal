//! `shoal run` with a command.
use std::ffi::OsString;

use anyhow::Result;

use crate::cli::{
    context::Context,
    ui::{self, Acting, Fallback},
};
use shoal_protocol::{runs::CommandKey, space_names::SpaceSelector};
use shoal_workers::execution::CommandRun;

pub(in crate::cli::commands) async fn exec(
    ctx: &Context,
    workspace: Option<SpaceSelector>,
    now: bool,
    command: Vec<OsString>,
) -> Result<i32> {
    let workspace =
        ui::select_workspace(ctx, workspace, Fallback::CurrentDirectory(&running())).await?;
    let argv = command.iter().map(|arg| arg.to_string_lossy().into_owned());
    let run = CommandRun {
        key: CommandKey::Argv(argv.collect()),
        now,
    };
    shoal_workers::execution::run_command(&ctx.paths, workspace, command, Some(run)).await
}

pub(in crate::cli::commands) fn running() -> Acting {
    Acting::here(crate::client::executions::command_run)
}
