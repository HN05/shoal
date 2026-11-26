//! `shoal edit`.
use std::ffi::OsString;

use anyhow::{Context as _, Result};

use crate::{
    cli::{
        commands::configured,
        context::Context,
        ui::{self, Acting, Fallback},
    },
    protocol::ConfigTarget,
};
use shoal_protocol::space_names::SpaceSelector;

const EDIT: &str = "edit";

pub(in crate::cli::commands) async fn edit(
    ctx: &Context,
    workspace: Option<SpaceSelector>,
    args: Vec<OsString>,
) -> Result<i32> {
    let workspace =
        ui::select_workspace(ctx, workspace, Fallback::CurrentDirectory(&editing())).await?;
    let settings =
        shoal_workers::settings::of(&ctx.paths, ConfigTarget::Space(workspace.clone())).await?;
    if settings.commands.contains_key(EDIT) {
        return configured::run(ctx, EDIT, Some(workspace.clone()), false, args).await;
    }
    let mut command = editor([std::env::var_os("VISUAL"), std::env::var_os("EDITOR")])?;
    command.push(
        crate::client::workspaces::inspect(workspace.clone())
            .send(&ctx.paths)
            .await?
            .workspace
            .part
            .path
            .into(),
    );
    command.extend(args);
    shoal_workers::execution::run_command(&ctx.paths, workspace, command, None).await
}

/// Split on whitespace, as in `code --wait`, unless it is not UTF-8.
fn editor(values: [Option<OsString>; 2]) -> Result<Vec<OsString>> {
    let value = values
        .into_iter()
        .flatten()
        .find(|value| !value.to_string_lossy().trim().is_empty())
        .context("set $VISUAL, $EDITOR or [commands] edit to open a workspace")?;
    Ok(match value.to_str() {
        Some(value) => value.split_whitespace().map(OsString::from).collect(),
        None => vec![value],
    })
}

pub(super) fn editing() -> Acting {
    Acting::here(crate::client::executions::command_run)
}

#[cfg(test)]
mod tests;
