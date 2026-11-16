//! Running a command or the editor in a workspace.
use std::ffi::OsString;

use anyhow::{Context as _, Result};

use crate::{
    cli::{
        commands::configured,
        context::Context,
        ui::{self, Fallback},
    },
    protocol::ConfigTarget,
};
use shoal_protocol::space_names::SpaceSelector;

pub(in crate::cli::commands) async fn exec(
    ctx: &Context,
    workspace: Option<SpaceSelector>,
    command: Vec<OsString>,
) -> Result<i32> {
    let workspace = ui::select_workspace(ctx, workspace, Fallback::CurrentDirectory).await?;
    shoal_workers::execution::run_command(&ctx.paths, workspace, command).await
}

/// The configured command `shoal edit` runs instead of `$VISUAL` or `$EDITOR`.
const EDIT: &str = "edit";

pub(in crate::cli::commands) async fn edit(
    ctx: &Context,
    workspace: Option<SpaceSelector>,
    args: Vec<OsString>,
) -> Result<i32> {
    let workspace = ui::select_workspace(ctx, workspace, Fallback::CurrentDirectory).await?;
    let settings =
        crate::config::settings(&ctx.paths, ConfigTarget::Space(workspace.clone())).await?;
    if settings.commands.contains_key(EDIT) {
        return configured::run(ctx, EDIT, Some(workspace.clone()), args).await;
    }
    let mut command = editor([std::env::var_os("VISUAL"), std::env::var_os("EDITOR")])?;
    command.push(
        crate::client::workspaces::inspect(&ctx.paths, workspace.clone())
            .await?
            .workspace
            .path
            .into(),
    );
    command.extend(args);
    shoal_workers::execution::run_command(&ctx.paths, workspace, command).await
}

/// The first set of `$VISUAL` and `$EDITOR` as an argument array: split on
/// whitespace, as in `code --wait`, unless it is not UTF-8.
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

#[cfg(test)]
mod tests;
