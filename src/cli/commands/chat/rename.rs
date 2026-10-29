//! Renaming a running chat.
use anyhow::Result;

use crate::{
    cli::{
        client::{self, request},
        commands::sessions,
        context::Context,
        ui,
    },
    model::Chat,
    protocol::ChatMethod,
};

pub(in crate::cli::commands) async fn rename(
    ctx: &Context,
    chat: Option<String>,
    name: String,
) -> Result<i32> {
    super::ensure_unscoped(ctx)?;
    let chat = select(ctx, chat).await?;
    let renamed = request::<Chat>(&ctx.paths, ChatMethod::RenameChat { chat, name }).await?;
    ctx.show(&renamed, |chat| outln!("Renamed chat to {}", chat.name))?;
    Ok(0)
}

/// The named chat, else the chat this process runs in, else the current
/// checkout's only running chat, else one from a picker, as a name or ID.
async fn select(ctx: &Context, chat: Option<String>) -> Result<String> {
    if let Some(chat) = chat.or_else(crate::env::chat) {
        return Ok(chat);
    }
    let chats = client::chats(&ctx.paths).await?;
    if let Some(chat) = sessions::checkout_chat(ctx, &chats).await? {
        return Ok(chat.id);
    }
    ui::pick_chat(ctx, &chats)
}
