//! `shoal rename`: rename a workspace or a chat, a picked one with what the
//! user types, or ask its agent to choose.
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

/// Rename the space `space` selects, else the space this runs in or a
/// picked one.
pub(super) async fn rename(
    ctx: &Context,
    space: Option<SpaceSelector>,
    name: String,
) -> Result<i32> {
    // Refuse a name before a picker asks for the space.
    let name = SpaceName::parse(&name)?;
    rename_space(ctx, space, name).await
}

/// Rename a picked workspace or chat with what the user types next.
pub(super) async fn rename_picked(ctx: &Context) -> Result<i32> {
    let space = ui::pick_any_space(ctx).await?;
    let name = ask_name(ctx).await?;
    rename_space(ctx, Some(space), name).await
}

/// Give the chat or workspace `space` names, else the one this runs in or
/// a picked one, the name `name`.
async fn rename_space(ctx: &Context, space: Option<SpaceSelector>, name: SpaceName) -> Result<i32> {
    match chosen_chat(ctx, space).await? {
        Ok(chat) => chat::rename(ctx, SpaceSelector::Id(chat), name).await,
        Err(workspace) => workspaces::set_name(ctx, Some(workspace), name).await,
    }
}

/// Ask for a space's new name until it is one a space can have.
async fn ask_name(ctx: &Context) -> Result<SpaceName> {
    ui::input_until(
        ctx,
        "Name",
        None,
        |name| async move { SpaceName::parse(&name) },
    )
    .await
}

/// Ask the agent of the space `space` selects, else the one this runs in or
/// a picked one, to choose its name.
pub(super) async fn request(ctx: &Context, space: Option<SpaceSelector>) -> Result<i32> {
    chat::ensure_scope_token(ctx)?;
    match chosen_chat(ctx, space).await? {
        Ok(chat) => chat::request_name(ctx, SpaceSelector::Id(chat)).await,
        Err(workspace) => workspaces::request_name(ctx, Some(workspace)).await,
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
