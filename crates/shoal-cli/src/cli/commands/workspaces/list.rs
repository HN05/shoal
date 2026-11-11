//! Listing workspaces, chats and queued agents.
use anyhow::Result;

use crate::cli::{
    commands::{chat, listing, queued_agents},
    context::Context,
    output::{self, Palette},
    ui,
};

pub(in crate::cli::commands) async fn list(
    ctx: &Context,
    filter: &listing::ListFilter,
) -> Result<i32> {
    // Listing chats first ends finished ones, returning their workspaces to the user.
    let chats = chat::listed(ctx, filter).await?;
    let mut workspaces = crate::client::workspaces::list(&ctx.paths).await?;
    let statuses = ui::Statuses {
        stopped: ui::stopped_workspaces(&ctx.paths, &workspaces),
        queued: queued_agents::listed(ctx, filter).await?,
    };
    workspaces.retain(|workspace| filter.keeps_workspace(workspace, &statuses.shown(workspace)));
    let rows = ui::workspace_tree_rows(
        &workspaces,
        &chats,
        &statuses,
        Palette::stdout(ctx.json),
        output::stdout_width(ctx.json),
    );
    ctx.show(&workspaces, |_| {
        for row in rows {
            outln!("{row}");
        }
    })?;
    Ok(0)
}
