//! Removing chats: each goes with the spaces below it, which the user sees
//! before confirming, and its agent stops.
use anyhow::Result;

use crate::{
    cli::context::Context,
    model::{Chat, Spaces},
};
use shoal_protocol::chats::RemovedChat;

/// What removing `chat` takes besides it, as `spaces` shows: the spaces
/// below it, with the chats in their worktrees, its running agent and its
/// holds.
pub(in crate::cli::commands) fn losses(chat: &Chat, spaces: &Spaces) -> Vec<String> {
    let mut lines = Vec::new();
    let below: Vec<String> = (spaces.taken_with(&chat.id).into_iter())
        .map(|space| space.name.into())
        .collect();
    if !below.is_empty() {
        lines.push(format!(
            "Removes the spaces below it first, whatever their work or holds; shoal restore brings each workspace back: {}",
            below.join(", ")
        ));
    }
    if chat.running {
        lines.push("Stops its agent".into());
    }
    lines.extend(super::super::holds::losses(&chat.holds));
    lines
}

/// Remove `chat` with the spaces below it that `spaces` shows, the only
/// ones the daemon takes.
pub(in crate::cli::commands) async fn remove(
    ctx: &Context,
    chat: &Chat,
    spaces: &Spaces,
) -> Result<RemovedChat> {
    let children = (spaces.taken_with(&chat.id).into_iter())
        .map(|space| space.id)
        .collect();
    crate::client::chats::remove(&ctx.paths, &chat.id, Some(children)).await
}

/// `removed`, such as `Chat removed`, with the spaces below it the removal
/// took.
pub(in crate::cli::commands) fn message(removed: &str, chat: &RemovedChat) -> String {
    match chat.children.as_slice() {
        [] => removed.to_owned(),
        children => format!(
            "{removed}; also removed {}",
            shoal_protocol::space_names::join(children, ", ")
        ),
    }
}
