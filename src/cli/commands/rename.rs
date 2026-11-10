//! `shoal rename`: title a workspace or rename a chat, or ask its
//! agent to choose.
use crate::client::space_ids::SpaceId;
use anyhow::Result;

use super::{chat, workspaces};
use crate::client::space_names::SpaceSelector;
use crate::{
    cli::{
        context::Context,
        ui::{self, Fallback},
    },
    client::space_names::SpaceName,
    model::PresentSpace,
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
        Ok(chat) => chat::rename(ctx, SpaceSelector::Id(chat), SpaceName::parse(&title)?).await,
        Err(workspace) => workspaces::title(ctx, Some(workspace), title).await,
    }
}

/// Ask the agent of the named space, else the one this runs in or a picked
/// one, to choose its title or name.
pub(super) async fn request(ctx: &Context, space: Option<SpaceSelector>) -> Result<i32> {
    chat::ensure_scope_token(ctx)?;
    match chosen_chat(ctx, space).await? {
        Ok(chat) => chat::request_name(ctx, SpaceSelector::Id(chat)).await,
        Err(workspace) => workspaces::request_title(ctx, Some(workspace)).await,
    }
}

/// The space `space` names, else the current workspace or the chat this runs
/// in, else the current checkout's only running chat, else the current
/// workspace or a picked space: a chat's ID, or else the workspace.
async fn chosen_chat(
    ctx: &Context,
    space: Option<SpaceSelector>,
) -> Result<Result<SpaceId, SpaceSelector>> {
    if space.is_none() && ui::current_chat(&None).is_none() {
        let chats = crate::client::chats::list(&ctx.paths).await?;
        if let Some(chat) = super::sessions::checkout_chat(ctx, &chats).await? {
            return Ok(Ok(chat.id));
        }
    }
    let space = ui::select_space(ctx, space, Fallback::CurrentDirectory).await?;
    Ok(
        match crate::client::workspaces::present_space(&ctx.paths, &space).await? {
            Some(PresentSpace::Chat(chat)) => Ok(chat.id),
            _ => Err(space),
        },
    )
}
