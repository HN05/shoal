//! `shoal convert`: a chat becomes a workspace, which the command sets up,
//! or a workspace a chat. The space stays the same, and its agent restarts
//! in the new place once its turn ends.
use anyhow::{Context as _, Result};

use super::issues::Issue;
use crate::client::output::convert::Converted;
use crate::client::space_names::SpaceSelector;
use crate::{
    cli::{
        args::create::BranchArgs,
        context::Context,
        output::{Palette, Style},
        ui::{self, Fallback},
    },
    config::modes::ModeName,
    model::{SpaceKind, Workspace},
    protocol::{CreationOptions, ItemContext},
};

/// Turn the chat `space` names, or the one this runs in, into a workspace
/// on the branch `branch` names, set it up, and report it.
pub(super) async fn to_workspace(
    ctx: &Context,
    space: Option<SpaceSelector>,
    branch: BranchArgs,
    mode: Option<ModeName>,
) -> Result<i32> {
    if let Some(branch) = &branch.branch {
        crate::git::check_branch_name(None, branch).await?;
    }
    let chat = ui::select_space(ctx, space, own_space()).await?;
    let issue = match &branch.issue {
        Some(input) => Some(load_issue(ctx, &chat, input).await?),
        None => None,
    };
    let name = match (branch.branch, &issue) {
        (Some(name), _) => name,
        (None, Some(issue)) => issue.branch_name(),
        (None, None) => linked_issue_branch(ctx, &chat).await?,
    };
    let new = crate::client::conversions::NewWorkspace {
        chat,
        branch: name,
        issue: issue.map(|issue| issue.url),
        base: branch.base,
        options: CreationOptions {
            skip_setup: branch.skip_setup,
            disposable: false,
        },
        mode,
    };
    let env = crate::env::without_secrets();
    let started = crate::client::conversions::to_workspace(&ctx.paths, new, env).await?;
    let Some(workspace) = super::split::follow_preparation(ctx, started).await? else {
        return Ok(1);
    };
    let text = format!(
        "Converted {} into a workspace {}; its agent restarts there once its turn ends",
        Palette::stdout(ctx.json).paint(Style::Heading, &workspace.name),
        super::workspaces::location_phrase(&workspace),
    );
    report(ctx, &text, SpaceKind::Workspace, workspace)
}

/// Turn the workspace `space` names, or the one this runs in, into a chat
/// once its agent's turn ends, and report it.
pub(super) async fn to_chat(
    ctx: &Context,
    space: Option<SpaceSelector>,
    mode: Option<ModeName>,
) -> Result<i32> {
    let workspace = ui::select_space(ctx, space, own_space()).await?;
    let workspace = crate::client::conversions::to_chat(&ctx.paths, workspace, mode).await?;
    let text = format!(
        "{} becomes a chat in the repository's checkout once its agent's turn ends; its branch {} stays",
        Palette::stdout(ctx.json).paint(Style::Heading, &workspace.name),
        workspace.branch.as_deref().unwrap_or("-"),
    );
    report(ctx, &text, SpaceKind::Chat, workspace)
}

/// Where a conversion's space comes from without `--space`: an agent's own
/// space, or the user's current workspace, else a picker.
fn own_space() -> Fallback {
    match crate::env::is_scoped() {
        true => Fallback::OwnWorkspace,
        false => Fallback::CurrentDirectory,
    }
}

/// The issue `input` names, read where the chat reads its forge.
async fn load_issue(ctx: &Context, chat: &SpaceSelector, input: &str) -> Result<Issue> {
    super::issues::load(ctx, ItemContext::Chat(chat.clone()), input).await
}

/// The branch name the issue linked to the chat `chat` gives, refusing a
/// chat without one.
async fn linked_issue_branch(ctx: &Context, chat: &SpaceSelector) -> Result<String> {
    let chats = crate::client::chats::list(&ctx.paths).await?;
    let linked = (chats.iter())
        .find(|candidate| chat.selects(&candidate.id, &candidate.name, candidate.number))
        .and_then(|chat| chat.links.issue.clone())
        .context("name the workspace's branch, or pass --issue or link the chat's issue first")?;
    Ok(load_issue(ctx, chat, &linked).await?.branch_name())
}

/// Print what the conversion did to `workspace`, which is or becomes a
/// space of `kind`.
fn report(ctx: &Context, text: &str, kind: SpaceKind, workspace: Workspace) -> Result<i32> {
    ctx.emit(text, Converted { kind, workspace })?;
    Ok(0)
}
