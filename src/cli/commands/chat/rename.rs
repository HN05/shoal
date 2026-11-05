//! Renaming a running chat, or asking its agent to, for `shoal work
//! rename`.
use anyhow::Result;

use crate::{
    cli::{context::Context, terminal},
    model::RenamedChat,
    protocol::ConfigTarget,
};

/// What `shoal work rename --request` asks a chat's agent.
const NAME_REQUEST: &str = "The user asks you to rename this chat: name the work as it stands now in two to five hyphenated words with `shoal work rename <name>`.";

/// Rename `chat`, by ID or name; the daemon lets a chat's agent rename only
/// its own chat.
pub(in crate::cli::commands) async fn rename(
    ctx: &Context,
    chat: String,
    name: String,
) -> Result<i32> {
    crate::validate::name("chat", &name)?;
    let renamed = crate::client::chats::rename(&ctx.paths, chat, name).await?;
    if let Err(error) = relabel(ctx, &renamed).await {
        eprintln!(
            "warning: cannot relabel the place of chat {}: {error:#}",
            renamed.chat.name
        );
    }
    ctx.show(&renamed.chat, |chat| {
        outln!("Renamed chat to {}", chat.name)
    })?;
    Ok(0)
}

/// Ask the agent of the running `chat`, by ID or name, to choose its name.
/// A chat runs as long as its agent, so the message always has a reader.
pub(in crate::cli::commands) async fn request_name(ctx: &Context, chat: String) -> Result<i32> {
    super::ensure_scope_token(ctx)?;
    crate::client::agent_messages::send(&ctx.paths, chat, NAME_REQUEST.into(), Vec::new(), None)
        .await?;
    ctx.emit("Asked the chat's agent to choose its name.", ())?;
    Ok(0)
}

/// Show the renamed chat's name on the place it runs in, where that still
/// shows its previous name; the chat keeps its new name when that fails.
async fn relabel(ctx: &Context, renamed: &RenamedChat) -> Result<()> {
    let RenamedChat {
        chat,
        previous_name,
    } = renamed;
    let Some(place) = &chat.origin else {
        return Ok(());
    };
    let target = ConfigTarget::chat(chat);
    let settings = crate::client::workspaces::settings(&ctx.paths, target).await?;
    let labels = terminal::Labels {
        new: chat.name.clone(),
        expected: previous_name.clone(),
    };
    let places = std::slice::from_ref(place);
    terminal::relabel_space(&settings, places, async |_| labels.clone()).await;
    Ok(())
}
