//! Removing chats: each goes with the workspaces below it, which the user
//! sees before confirming, and its agent stops.
use anyhow::Result;

use crate::{
    cli::{client, context::Context},
    client::output::chat::RemovedChat,
    model::{Chat, Spaces},
};

/// What removing `chat` takes besides it, as `spaces` shows: the
/// workspaces below it and its running agent.
pub(in crate::cli::commands) fn losses(chat: &Chat, spaces: &Spaces) -> Vec<String> {
    let mut lines = Vec::new();
    let below: Vec<&str> = (spaces.below_chat(&chat.id).iter())
        .map(|workspace| workspace.name.as_str())
        .collect();
    if !below.is_empty() {
        lines.push(format!(
            "Removes the workspaces below it first, whatever their work or holds; shoal work restore brings each back: {}",
            below.join(", ")
        ));
    }
    if chat.restore.is_none() {
        lines.push("Stops its agent".into());
    }
    lines
}

/// Remove `chat` with the workspaces below it that `spaces` shows, the only
/// ones the daemon takes.
pub(in crate::cli::commands) async fn remove(
    ctx: &Context,
    chat: &Chat,
    spaces: &Spaces,
) -> Result<RemovedChat> {
    let children = (spaces.below_chat(&chat.id).iter())
        .map(|workspace| workspace.id.clone())
        .collect();
    client::remove_chat(&ctx.paths, &chat.id, Some(children)).await
}

/// `removed`, such as `Chat removed`, with the workspaces below it the
/// removal took.
pub(in crate::cli::commands) fn message(removed: &str, chat: &RemovedChat) -> String {
    match chat.children.as_slice() {
        [] => removed.to_owned(),
        children => format!("{removed}; also removed {}", children.join(", ")),
    }
}
