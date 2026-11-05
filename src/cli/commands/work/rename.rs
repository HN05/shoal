//! `shoal work rename`: title a workspace or rename a chat, or ask its
//! agent to choose.
use anyhow::Result;

use super::super::{chat, workspaces};
use crate::{
    cli::{
        client,
        context::Context,
        ui::{self, Fallback},
    },
    model::PresentSpace,
};

/// Title the named workspace, or rename the named chat, else the space this
/// runs in or a picked one.
pub(super) async fn rename(ctx: &Context, space: Option<String>, title: String) -> Result<i32> {
    // Refuse what neither a title nor a chat's name may be before a picker
    // asks for the space.
    if let Err(error) = crate::validate::title(title.trim()) {
        crate::validate::name("chat", &title).map_err(|_| error)?;
    }
    match chosen_chat(ctx, space).await? {
        Ok(chat) => chat::rename(ctx, chat, title).await,
        Err(workspace) => workspaces::title(ctx, Some(workspace), title).await,
    }
}

/// Ask the agent of the named space, else the one this runs in or a picked
/// one, to choose its title or name.
pub(super) async fn request(ctx: &Context, space: Option<String>) -> Result<i32> {
    chat::ensure_scope_token(ctx)?;
    match chosen_chat(ctx, space).await? {
        Ok(chat) => chat::request_name(ctx, chat).await,
        Err(workspace) => workspaces::request_title(ctx, Some(workspace)).await,
    }
}

/// The space `space` names, else the chat this runs in, else the current
/// checkout's only running chat, else the current workspace or a picked
/// space: a chat's ID, or else the workspace.
async fn chosen_chat(ctx: &Context, space: Option<String>) -> Result<Result<String, String>> {
    if space.is_none() && ui::current_chat(&None).is_none() {
        let chats = client::chats(&ctx.paths).await?;
        if let Some(chat) = super::super::sessions::checkout_chat(ctx, &chats).await? {
            return Ok(Ok(chat.id));
        }
    }
    let space = ui::select_space(ctx, space, Fallback::CurrentDirectory).await?;
    Ok(match client::present_space(&ctx.paths, &space).await? {
        Some(PresentSpace::Chat(chat)) => Ok(chat.id),
        _ => Err(space),
    })
}
