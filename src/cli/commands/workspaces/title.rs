//! Titling a workspace, which the places Shoal opened for it show.
use anyhow::Result;

use crate::{
    cli::{
        client::request,
        context::Context,
        terminal,
        ui::{self, Fallback},
    },
    model::Workspace,
    protocol::WorkspaceMethod,
};

pub(in crate::cli::commands) async fn title(
    ctx: &Context,
    workspace: Option<String>,
    title: String,
) -> Result<i32> {
    // Refuse an invalid title before a picker asks for the workspace.
    crate::validate::title(title.trim())?;
    let workspace = ui::select_workspace(ctx, workspace, Fallback::CurrentDirectory).await?;
    let titled = request::<Workspace>(
        &ctx.paths,
        WorkspaceMethod::SetWorkspaceTitle {
            workspace,
            title: Some(title),
        },
    )
    .await?;
    // The title is recorded; a place left with its old label only warns.
    if let Err(error) = terminal::relabel_places(ctx, &titled).await {
        eprintln!(
            "warning: cannot relabel the places of {}: {error:#}",
            titled.name
        );
    }
    ctx.show(&titled, |workspace| {
        outln!(
            "Set the title of {} to {}",
            workspace.name,
            workspace.title.as_deref().unwrap_or_default()
        )
    })?;
    Ok(0)
}
