//! `shoal work rename`: title a workspace or rename a chat, or ask its
//! agent to choose.
use anyhow::Result;

use super::super::{chat, workspaces};
use crate::client::space_names::SpaceSelector;
use crate::{
    cli::{
        context::Context,
        ui::{self, Fallback},
    },
    client::space_names::SpaceName,
    model::{PresentSpace, SpaceKind},
};

/// Title the named workspace, or rename the named chat, else the space this
/// runs in or a picked one.
pub(super) async fn rename(
    ctx: &Context,
    space: Option<SpaceSelector>,
    title: String,
) -> Result<i32> {
    // Refuse what neither a title nor a chat's name may be before a picker
    // asks for the space.
    if let Err(error) = crate::validate::title(title.trim()) {
        SpaceName::parse(&title).map_err(|_| error)?;
    }
    match chosen_chat(ctx, space).await? {
        Ok(chat) => chat::rename(ctx, chat, chat_name(&title)?).await,
        Err(workspace) => {
            workspaces::title(ctx, Some(SpaceSelector::parse(&workspace)), title).await
        }
    }
}

/// The name a chat takes from `text`: `text` itself when it is a valid name,
/// else the name its words give as a title.
fn chat_name(text: &str) -> Result<SpaceName> {
    SpaceName::parse(text)
        .or_else(|refused| SpaceName::from_title(SpaceKind::Chat, text.trim()).ok_or(refused))
}

/// Ask the agent of the named space, else the one this runs in or a picked
/// one, to choose its title or name.
pub(super) async fn request(ctx: &Context, space: Option<SpaceSelector>) -> Result<i32> {
    chat::ensure_scope_token(ctx)?;
    match chosen_chat(ctx, space).await? {
        Ok(chat) => chat::request_name(ctx, chat).await,
        Err(workspace) => {
            workspaces::request_title(ctx, Some(SpaceSelector::parse(&workspace))).await
        }
    }
}

/// The space `space` names, else the current workspace or the chat this runs
/// in, else the current checkout's only running chat, else the current
/// workspace or a picked space: a chat's ID, or else the workspace.
async fn chosen_chat(
    ctx: &Context,
    space: Option<SpaceSelector>,
) -> Result<Result<String, String>> {
    if space.is_none() && ui::current_chat(&None).is_none() {
        let chats = crate::client::chats::list(&ctx.paths).await?;
        if let Some(chat) = super::super::sessions::checkout_chat(ctx, &chats).await? {
            return Ok(Ok(chat.id));
        }
    }
    let space = ui::select_space(ctx, space, Fallback::CurrentDirectory).await?;
    Ok(
        match crate::client::workspaces::present_space(&ctx.paths, &SpaceSelector::parse(&space))
            .await?
        {
            Some(PresentSpace::Chat(chat)) => Ok(chat.id),
            _ => Err(space),
        },
    )
}
