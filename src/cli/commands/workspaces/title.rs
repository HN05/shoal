//! Titling a workspace, which the places Shoal opened for it show, or
//! asking its agent to.
use anyhow::{Result, ensure};

use crate::cli::{
    commands::turns,
    context::Context,
    terminal,
    ui::{self, Fallback},
};

/// What `shoal work rename --request` asks the workspace's agent.
const TITLE_REQUEST: &str = "The user asks you to retitle this workspace: name the work as it stands now in two to five words with `shoal work rename \"<title>\"`.";

pub(in crate::cli::commands) async fn title(
    ctx: &Context,
    workspace: Option<String>,
    title: String,
) -> Result<i32> {
    // Refuse an invalid title before a picker asks for the workspace.
    crate::validate::title(title.trim())?;
    let workspace = ui::select_workspace(ctx, workspace, Fallback::CurrentDirectory).await?;
    let titled = crate::client::workspaces::set_title(&ctx.paths, workspace, title).await?;
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

/// Ask the workspace's running agent to choose its title, through the
/// messages `shoal messages send` queues. Without an agent the message
/// would wait unread until removal, so that is refused.
pub(in crate::cli::commands) async fn request_title(
    ctx: &Context,
    workspace: Option<String>,
) -> Result<i32> {
    let selector = ui::select_workspace(ctx, workspace, Fallback::CurrentDirectory).await?;
    let (workspace, agents) = turns::agents_of(ctx, selector).await?;
    ensure!(
        !agents.is_empty(),
        "{name} has no running agent to ask; set its title with shoal work rename {name} <TITLE>",
        name = workspace.name
    );
    let message = TITLE_REQUEST.into();
    crate::client::agent_messages::send(&ctx.paths, workspace.id, message, Vec::new(), None)
        .await?;
    ctx.emit("Asked the workspace's agent to choose its title.", ())?;
    Ok(0)
}
