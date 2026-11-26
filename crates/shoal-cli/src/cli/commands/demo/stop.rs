//! `shoal demo stop`.
use anyhow::Result;

use crate::client::output::demo::Stopping;
use crate::{
    cli::{
        context::Context,
        output::Style,
        ui::{self, Acting, Fallback},
    },
    demos,
    model::Workspace,
};
use shoal_protocol::space_names::SpaceSelector;

pub(super) async fn stop(
    ctx: &Context,
    workspace: Option<SpaceSelector>,
    name: Option<String>,
) -> Result<i32> {
    let id = ui::select_workspace(ctx, workspace, Fallback::CurrentDirectory(&stopping())).await?;
    let workspace = crate::client::workspaces::inspect(id.clone())
        .send(&ctx.paths)
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

pub(super) fn stopping() -> Acting {
    Acting::request(crate::client::workspaces::inspect)
}
