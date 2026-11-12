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
    let mut statuses = ui::Statuses {
        stopped: ui::stopped_workspaces(&ctx.paths).await,
        queued: queued_agents::entries(ctx).await?,
    };
    workspaces.retain(|workspace| filter.keeps_workspace(workspace, &statuses.shown(workspace)));
    (statuses.queued).retain(|entry| filter.keeps_listed_entry(entry, &workspaces));
    let repositories = crate::client::repositories::list(&ctx.paths).await?;
    let rows = ui::workspace_tree_rows(
        &workspaces,
        &chats,
        &statuses,
        &ui::repository_choices(repositories).await,
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
