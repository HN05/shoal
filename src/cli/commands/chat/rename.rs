//! Renaming a running chat, or asking its agent to, for `shoal
//! rename`.
use anyhow::Result;

use crate::client::space_names::SpaceSelector;
use crate::{
    cli::{context::Context, terminal, ui},
    client::space_names::SpaceName,
    model::RenamedChat,
    protocol::ConfigTarget,
};

/// What `shoal rename --request` asks a chat's agent.
const NAME_REQUEST: &str = "The user asks you to rename this chat: name the work as it stands now in two to five hyphenated words with `shoal rename <name>`.";

/// Rename `chat`, by ID or name; the daemon lets a chat's agent rename only
/// its own chat.
pub(in crate::cli::commands) async fn rename(
    ctx: &Context,
    chat: SpaceSelector,
    name: SpaceName,
) -> Result<i32> {
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
pub(in crate::cli::commands) async fn request_name(
    ctx: &Context,
    chat: SpaceSelector,
) -> Result<i32> {
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
        new: chat.name.to_string(),
        expected: previous_name.to_string(),
    };
    let places = std::slice::from_ref(place);
    terminal::relabel_space(&settings, places, async |_| labels.clone()).await;
    Ok(())
}

/// Ask for the chat's new name until the daemon accepts one.
pub(in crate::cli::commands) async fn ask_name(ctx: &Context) -> Result<SpaceName> {
    ui::input_until(ctx, "Chat name", None, |name| async move {
        let name = SpaceName::parse(&name)?;
        crate::client::chats::check_name(&ctx.paths, &name).await?;
        Ok(name)
    })
    .await
}
