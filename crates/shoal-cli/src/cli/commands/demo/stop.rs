//! `shoal demo stop`.
use anyhow::Result;

use crate::client::output::demo::Stopping;
use crate::client::space_names::SpaceSelector;
use crate::{
    cli::{
        context::Context,
        output::Style,
        ui::{self, Fallback},
    },
    demos,
    model::Workspace,
};

pub(super) async fn stop(
    ctx: &Context,
    workspace: Option<SpaceSelector>,
    name: Option<String>,
) -> Result<i32> {
    let id = ui::select_workspace(ctx, workspace, Fallback::CurrentDirectory).await?;
    let workspace = crate::client::workspaces::inspect(&ctx.paths, id.clone())
        .await?
        .workspace;
    stop_named(ctx, &workspace, name.clone()).await?;
    let message = match &name {
        Some(name) => format!("Stopping demo {name}"),
        None => format!("Stopping the demos of {}", workspace.name),
    };
    ctx.emit_styled(
        Style::Success,
        &message,
        Stopping {
            stopping: true,
            name,
        },
    )?;
    Ok(0)
}

pub(super) async fn stop_named(
    ctx: &Context,
    workspace: &Workspace,
    name: Option<String>,
) -> Result<()> {
    demos::stop(&ctx.paths, SpaceSelector::id(&workspace.id), name).await
}
