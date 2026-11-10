//! Listing chats, for `shoal work --chats`.
use anyhow::Result;

use crate::{
    cli::{
        commands::listing::ListFilter,
        context::Context,
        output::{self, Palette},
        ui,
    },
    model::Chat,
};

/// `shoal work --chats`.
pub(in crate::cli::commands) async fn list(ctx: &Context, filter: &ListFilter) -> Result<i32> {
    let chats = listed(ctx, filter).await?;
    let rows = ui::workspace_tree_rows(
        &[],
        &chats,
        &Default::default(),
        Palette::stdout(ctx.json),
        output::stdout_width(ctx.json),
    );
    ctx.show(&chats, |_| {
        for row in rows {
            outln!("{row}");
        }
    })?;
    Ok(0)
}

/// The running and stopped chats `filter` keeps.
pub(in crate::cli::commands) async fn listed(
    ctx: &Context,
    filter: &ListFilter,
) -> Result<Vec<Chat>> {
    let mut chats = crate::client::chats::recorded(&ctx.paths).await?;
    chats.retain(|chat| filter.keeps_chat(chat));
    Ok(chats)
}
