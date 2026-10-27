//! Listing workspaces and queued agents.
use anyhow::Result;

use crate::{
    cli::{
        client,
        commands::{agent_queue, chat, listing},
        context::Context,
        output::{self, Palette},
        ui,
    },
    env,
};

pub(in crate::cli::commands) async fn list(
    ctx: &Context,
    queue: bool,
    filter: &listing::ListFilter,
) -> Result<i32> {
    if queue {
        return agent_queue::list(ctx, filter).await;
    }
    // Listing chats first ends finished ones, returning their workspaces to the user.
    let chats = chat::listed(ctx, filter).await?;
    let mut workspaces = client::workspaces(&ctx.paths).await?;
    let stopped = ui::stopped_workspaces(&ctx.paths, &workspaces);
    workspaces
        .retain(|workspace| filter.keeps_workspace(workspace, stopped.contains(&workspace.id)));
    let queued = agent_queue::rows(ctx, &agent_queue::listed(ctx, filter).await?).await?;
    let rows = ui::workspace_tree_rows(
        &workspaces,
        &chats,
        &stopped,
        Palette::stdout(ctx.json),
        output::stdout_width(ctx.json),
    );
    ctx.show(&workspaces, |_| {
        for row in rows.into_iter().chain(queued) {
            outln!("{row}");
        }
    })?;
    // A pointer for humans; agents cannot read notifications and JSON stays clean.
    if !ctx.json
        && !env::is_scoped()
        && let Ok(Some(status)) = client::status(&ctx.paths).await
        && status.unread_notifications > 0
    {
        eprintln!(
            "{} new notification{}; run shoal alerts",
            status.unread_notifications,
            if status.unread_notifications == 1 {
                ""
            } else {
                "s"
            }
        );
    }
    Ok(0)
}
