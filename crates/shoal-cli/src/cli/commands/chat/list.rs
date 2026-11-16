//! Listing chats, for `shoal list --chats`.
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

/// `shoal list --chats`.
pub(in crate::cli::commands) async fn list(ctx: &Context, filter: &ListFilter) -> Result<i32> {
    let spaces = crate::client::spaces::list(&ctx.paths).await?;
    let chats = kept(spaces.into_chats(), filter);
    let rows = ui::workspace_tree_rows(
        &[],
        &chats,
        &Default::default(),
        &[],
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

/// The chats among `chats`, running or stopped, that `filter` keeps.
pub(in crate::cli::commands) fn kept(mut chats: Vec<Chat>, filter: &ListFilter) -> Vec<Chat> {
    chats.retain(|chat| filter.keeps_chat(chat));
    chats
}
