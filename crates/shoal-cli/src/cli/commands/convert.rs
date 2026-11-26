//! `shoal convert`.
use anyhow::{Context as _, Result};
use shoal_refs::BranchName;

use super::issues::Issue;
use crate::client::output::convert::Converted;
use crate::client::preparation::Preparing;
use crate::{
    cli::{
        args::create::BranchArgs,
        context::Context,
        output::{Palette, Style},
        ui::{self, Acting, Fallback},
    },
    config::modes::ModeName,
    forge::items::reference::ItemInput,
    model::{SpaceKind, Workspace},
    protocol::{AgentChange, CreationOptions, ItemContext},
};
use shoal_protocol::space_names::SpaceSelector;

pub(super) async fn to_workspace(
    ctx: &Context,
    space: Option<SpaceSelector>,
    branch: BranchArgs,
    mode: Option<ModeName>,
    agent: AgentChange,
) -> Result<i32> {
    // A supplied branch fails before the space picker opens.
    if let Some(branch) = &branch.branch {
        shoal_core::git::check_branch_name(branch)?;
    }
    let converting = to_workspace_request(mode.clone(), agent.clone());
    let chat = ui::select_space(ctx, space, own_space(&converting)).await?;
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
        issue: issue.map(|issue| issue.url.into()),
        base: branch.base,
        // The workspace keeps the chat's name.
        options: CreationOptions {
            skip_setup: branch.skip_setup,
            disposable: false,
            ..Default::default()
        },
        mode,
        agent,
    };
    let env = crate::env::without_secrets();
    let started = (crate::client::conversions::to_workspace(new, env))
        .send(&ctx.paths)
        .await?;
    let preparing = Preparing::follow(&ctx.paths, started).await?;
    let Some(workspace) = super::create::set_up(ctx, preparing).await? else {
        return Ok(1);
    };
    let text = format!(
        "Converted {} into a workspace {}; its agent restarts there once its turn ends",
        Palette::stdout(ctx.json).paint(Style::Heading, &workspace.name),
        super::preparation::location_phrase(&workspace),
    );
    report(ctx, &text, SpaceKind::Workspace, workspace)
}

pub(super) async fn to_chat(
    ctx: &Context,
    space: Option<SpaceSelector>,
    mode: Option<ModeName>,
    agent: AgentChange,
) -> Result<i32> {
    let converting = to_chat_request(mode.clone(), agent.clone());
    let workspace = ui::select_space(ctx, space, own_space(&converting)).await?;
    let workspace = (crate::client::conversions::to_chat(workspace, mode, agent))
        .send(&ctx.paths)
        .await?;
    let text = format!(
        "{} becomes a chat in the repository's checkout once its agent's turn ends; its branch {} stays",
        Palette::stdout(ctx.json).paint(Style::Heading, &workspace.name),
        workspace.branch().map(BranchName::as_str).unwrap_or("-"),
    );
    report(ctx, &text, SpaceKind::Chat, workspace)
}

pub(super) async fn to_agent(
    ctx: &Context,
    space: Option<SpaceSelector>,
    mode: Option<ModeName>,
    agent: AgentChange,
) -> Result<i32> {
    let switching = to_agent_request(mode.clone(), agent.clone());
    let space = ui::select_space(ctx, space, own_space(&switching)).await?;
    let switched = (crate::client::conversions::to_agent(space, mode, agent))
        .send(&ctx.paths)
        .await?;
    let name = Palette::stdout(ctx.json).paint(Style::Heading, &switched.space.name);
    let to = &switched.to;
    let text = match switched.at_once {
        true => format!(
            "{to} takes over the work of {name} now, from copies of its sessions in its attached files"
        ),
        false => format!(
            "{to} takes over the work of {name} once its agent's turn ends, from copies of its sessions in its attached files"
        ),
    };
    ctx.emit(&text, *switched)?;
    Ok(0)
}

/// Where a conversion's space comes from without `--space`: an agent's own
/// space, or the user's current workspace, else a picker.
fn own_space(acting: &Acting) -> Fallback<'_> {
    match crate::env::is_scoped() {
        true => Fallback::OwnSpace,
        false => Fallback::CurrentDirectory(acting),
    }
}

async fn load_issue(ctx: &Context, chat: &SpaceSelector, input: &ItemInput) -> Result<Issue> {
    super::issues::load(ctx, ItemContext::space(chat.clone()), input.clone()).await
}

/// The branch name the issue linked to the chat `chat` gives, refusing a
/// chat without one.
async fn linked_issue_branch(ctx: &Context, chat: &SpaceSelector) -> Result<String> {
    let chats = crate::client::spaces::list(&ctx.paths)
        .await?
        .into_running_chats();
    let linked = (chats.iter())
        .find(|candidate| chat.selects(&candidate.id, candidate.number))
        .and_then(|chat| Some(chat.links.issue()?.url.clone()))
        .context("name the workspace's branch, or pass --issue or link the chat's issue first")?;
    Ok(load_issue(ctx, chat, &linked.into()).await?.branch_name())
}

fn report(ctx: &Context, text: &str, kind: SpaceKind, workspace: Workspace) -> Result<i32> {
    ctx.emit(text, Converted { kind, workspace })?;
    Ok(0)
}

/// The branch, which may still come from the chat's issue, decides nothing of
/// which spaces the conversion reaches.
pub(super) fn to_workspace_request(mode: Option<ModeName>, agent: AgentChange) -> Acting {
    Acting::request(move |chat| {
        let new = crate::client::conversions::NewWorkspace {
            chat,
            branch: "branch".into(),
            issue: None,
            base: None,
            options: CreationOptions::default(),
            mode: mode.clone(),
            agent: agent.clone(),
        };
        crate::client::conversions::to_workspace(new, Default::default())
    })
}

pub(super) fn to_chat_request(mode: Option<ModeName>, agent: AgentChange) -> Acting {
    Acting::request(move |space| {
        crate::client::conversions::to_chat(space, mode.clone(), agent.clone())
    })
}

pub(super) fn to_agent_request(mode: Option<ModeName>, agent: AgentChange) -> Acting {
    Acting::request(move |space| {
        crate::client::conversions::to_agent(space, mode.clone(), agent.clone())
    })
}
