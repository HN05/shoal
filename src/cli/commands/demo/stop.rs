//! `shoal demo stop`, and stopping a demo that a start replaces.
use anyhow::{Result, ensure};

use super::{
    lookup::running,
    start::{START_LIMIT, START_POLL},
};
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

/// Stop a running demo `name` and wait for it to end, so the new one neither
/// shares its place nor is mistaken for it.
pub(super) async fn replace(ctx: &Context, workspace: &Workspace, name: &str) -> Result<()> {
    stop_named(ctx, workspace, Some(name.to_owned())).await?;
    let deadline = tokio::time::Instant::now() + START_LIMIT;
    while running(ctx, Some(workspace))
        .await?
        .iter()
        .any(|demo| demo.launch.name == name)
    {
        ensure!(
            tokio::time::Instant::now() < deadline,
            "demo {name} is still stopping; start it again once `shoal demo ls` no longer lists it"
        );
        tokio::time::sleep(START_POLL).await;
    }
    Ok(())
}

async fn stop_named(ctx: &Context, workspace: &Workspace, name: Option<String>) -> Result<()> {
    demos::stop(&ctx.paths, SpaceSelector::id(&workspace.id), name).await
}
