//! Renaming a running chat.
use anyhow::Result;

use crate::{
    cli::{
        client::{self, request},
        commands::sessions,
        context::Context,
        terminal, ui,
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
    // Refuse an invalid name before a picker asks for the chat.
    crate::validate::name("chat", &name)?;
    let chat = select(ctx, chat).await?;
    let old = name_of(ctx, &chat).await?;
    let renamed = request::<Chat>(&ctx.paths, ChatMethod::RenameChat { chat, name }).await?;
    relabel(ctx, old.as_deref(), &renamed).await;
    ctx.show(&renamed, |chat| outln!("Renamed chat to {}", chat.name))?;
    Ok(0)
}

/// The name of the running chat `chat` names by name or ID, if any.
async fn name_of(ctx: &Context, chat: &str) -> Result<Option<String>> {
    let chats = client::chats(&ctx.paths).await?;
    Ok(chats
        .into_iter()
        .find(|running| running.id == chat || running.name == chat)
        .map(|running| running.name))
}

/// Show the renamed chat's name on the place it runs in, where that still
/// shows `old`; the chat keeps its new name when that fails.
async fn relabel(ctx: &Context, old: Option<&str>, renamed: &Chat) {
    let Some(old) = old else {
        return;
    };
    if let Err(error) = terminal::relabel_chat(ctx, old, renamed).await {
        eprintln!(
            "warning: cannot relabel the place of chat {}: {error:#}",
            renamed.name
        );
    }
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
