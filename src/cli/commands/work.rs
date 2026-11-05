//! Workspace maintenance commands beneath `shoal work`.
use anyhow::Result;

use crate::{
    cli::{
        args::workspace::WorkCommand,
        client,
        context::Context,
        ui::{self, Fallback},
    },
    model::PresentSpace,
};

use super::{chat, restore, workspaces};

pub(super) async fn run(ctx: &Context, command: WorkCommand) -> Result<i32> {
    match command {
        WorkCommand::Env { workspace, revoke } => {
            workspaces::environment(ctx, workspace, revoke).await
        }
        WorkCommand::Adopt {
            path,
            repository,
            copy,
        } => workspaces::adopt(ctx, path, repository, copy).await,
        WorkCommand::Rename {
            space,
            title: Some(title),
            request: false,
        } => rename(ctx, space, title).await,
        // Only --request leaves out the title, and its single argument
        // takes the title's place.
        WorkCommand::Rename { space, title, .. } => request_rename(ctx, title.or(space)).await,
        WorkCommand::Checkpoint { workspace, name } => {
            restore::checkpoint(ctx, workspace, name).await
        }
        WorkCommand::Restore {
            workspace,
            checkpoint,
        } => restore::run(ctx, workspace, checkpoint).await,
        WorkCommand::Setup {
            workspace,
            skip: false,
        } => workspaces::setup(ctx, workspace).await,
        WorkCommand::Setup {
            workspace,
            skip: true,
        } => workspaces::skip_setup(ctx, workspace).await,
    }
}

/// Title the named workspace, or rename the named chat, else the space this
/// runs in or a picked one.
async fn rename(ctx: &Context, space: Option<String>, title: String) -> Result<i32> {
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
async fn request_rename(ctx: &Context, space: Option<String>) -> Result<i32> {
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
        if let Some(chat) = super::sessions::checkout_chat(ctx, &chats).await? {
            return Ok(Ok(chat.id));
        }
    }
    let space = ui::select_space(ctx, space, Fallback::CurrentDirectory).await?;
    Ok(match client::present_space(&ctx.paths, &space).await? {
        Some(PresentSpace::Chat(chat)) => Ok(chat.id),
        _ => Err(space),
    })
}
