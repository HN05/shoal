//! Renaming a running chat, or asking its agent to.
use anyhow::Result;

use crate::{
    cli::{
        client::{self, request},
        commands::sessions,
        context::Context,
        terminal, ui,
    },
    model::RenamedChat,
    protocol::{AgentMethod, ChatMethod},
};

/// What `shoal chat rename --request` asks the chat's agent.
const NAME_REQUEST: &str = "The user asks you to rename this chat: name the work as it stands now in two to five hyphenated words with `shoal chat rename <name>`.";

pub(in crate::cli::commands) async fn rename(
    ctx: &Context,
    chat: Option<String>,
    name: String,
) -> Result<i32> {
    super::ensure_unscoped(ctx)?;
    // Refuse an invalid name before a picker asks for the chat.
    crate::validate::name("chat", &name)?;
    let chat = select(ctx, chat).await?;
    let renamed = request::<RenamedChat>(&ctx.paths, ChatMethod::RenameChat { chat, name }).await?;
    relabel(ctx, &renamed).await;
    ctx.show(&renamed.chat, |chat| {
        outln!("Renamed chat to {}", chat.name)
    })?;
    Ok(0)
}

/// Ask the running chat's agent to choose its name. A chat runs as long as
/// its agent, so the message always has a reader.
pub(in crate::cli::commands) async fn request_name(
    ctx: &Context,
    chat: Option<String>,
) -> Result<i32> {
    super::ensure_unscoped(ctx)?;
    let chat = select(ctx, chat).await?;
    let method = AgentMethod::SendAgentMessage {
        space: chat,
        message: NAME_REQUEST.into(),
        attachments: Vec::new(),
        sender_chat: None,
    };
    request::<()>(&ctx.paths, method).await?;
    ctx.emit("Asked the chat's agent to choose its name.", ())?;
    Ok(0)
}

/// Show the renamed chat's name on the place it runs in, where that still
/// shows its previous name; the chat keeps its new name when that fails.
async fn relabel(ctx: &Context, renamed: &RenamedChat) {
    let RenamedChat {
        chat,
        previous_name,
    } = renamed;
    if let Err(error) = terminal::relabel_chat(ctx, previous_name, chat).await {
        eprintln!(
            "warning: cannot relabel the place of chat {}: {error:#}",
            chat.name
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
