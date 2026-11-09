//! `--permission` on `shoal resume` and `shoal restart`: a narrowing that
//! replaces the one the restored or restarted agents and chats recorded.
//! Each one's mode bounds it, which is checked for every selected space
//! before any narrowing is replaced.
use anyhow::{Context as _, Result};

use super::targets::Selected;
use crate::client::space_names::SpaceSelector;
use crate::execution::recovery::{LastSessionExt, RecordExt};
use crate::{
    cli::context::Context,
    config::permissions::{PermissionsConfig, SpaceMode},
    execution::recovery::{LastSession, Record, Saved},
    model::{Chat, SpaceKind, Workspace},
    protocol::ConfigTarget,
};

/// Give the stopped agents and last sessions of the `selected` workspaces,
/// only `execution`'s when given, `narrowing` in place of the one they
/// recorded, once it fits their modes and those of the selected chats,
/// which take it as they restore.
pub(super) async fn replace(
    ctx: &Context,
    selected: &Selected,
    execution: Option<&str>,
    narrowing: PermissionsConfig,
) -> Result<()> {
    let selectors = (selected.workspaces.as_deref())
        .context("--permission replaces the narrowing of the spaces a resume names")?;
    let mut stopped = Vec::new();
    for selector in selectors {
        let (workspace, saved) = saved_agents(ctx, selector, execution).await?;
        for mode in recorded_modes(&saved)? {
            ensure_fits_workspace(ctx, &workspace, &mode, &narrowing).await?;
        }
        stopped.push((workspace, saved));
    }
    for chat in &selected.chats {
        ensure_fits_chat(ctx, chat, &narrowing).await?;
    }
    for (workspace, saved) in &stopped {
        replace_saved(ctx, workspace, saved, narrowing).await?;
    }
    Ok(())
}

/// The workspace `selector` names with the agents and last sessions it
/// saved, only `execution`'s when given.
async fn saved_agents(
    ctx: &Context,
    selector: &str,
    execution: Option<&str>,
) -> Result<(Workspace, Saved)> {
    let workspace = crate::client::workspaces::inspect(&ctx.paths, SpaceSelector::parse(selector))
        .await?
        .workspace;
    let mut saved = Saved::load(&ctx.paths, &workspace.id)?;
    if let Some(execution) = execution {
        saved.retain_execution(execution, false);
    }
    Ok((workspace, saved))
}

/// Have the daemon give each agent and last session `saved` keeps of
/// `workspace` `narrowing` in place of the one it recorded.
async fn replace_saved(
    ctx: &Context,
    workspace: &Workspace,
    saved: &Saved,
    narrowing: PermissionsConfig,
) -> Result<()> {
    for (execution, _) in saved.agents.iter().chain(&saved.sessions) {
        crate::client::assignments::replace_stopped_narrowing(
            &ctx.paths,
            SpaceSelector::id(&workspace.id),
            execution.clone(),
            narrowing,
        )
        .await?;
    }
    Ok(())
}

/// Refuse `narrowing` where it is wider than the permissions of `mode`, the
/// mode of an agent of `workspace`.
pub(in crate::cli::commands) async fn ensure_fits_workspace(
    ctx: &Context,
    workspace: &Workspace,
    mode: &str,
    narrowing: &PermissionsConfig,
) -> Result<()> {
    let settings = crate::client::workspaces::settings(
        &ctx.paths,
        ConfigTarget::Workspace(SpaceSelector::id(&workspace.id)),
    )
    .await?;
    let mode = SpaceMode {
        name: mode,
        kind: SpaceKind::Workspace,
        repository: true,
    };
    (narrowing.ensure_within_mode(&settings, mode))
        .with_context(|| format!("the agent of {}", workspace.name))
}

/// Refuse `narrowing` where it is wider than the permissions of `chat`'s
/// mode, which a restore or restart keeps.
pub(in crate::cli::commands) async fn ensure_fits_chat(
    ctx: &Context,
    chat: &Chat,
    narrowing: &PermissionsConfig,
) -> Result<()> {
    let repo = super::chats::saved_repository(ctx, chat).await?;
    let settings = crate::client::workspaces::settings(&ctx.paths, repo.config()).await?;
    let mode = SpaceMode {
        name: &chat.mode,
        kind: SpaceKind::Chat,
        repository: chat.repository_id.is_some(),
    };
    (narrowing.ensure_within_mode(&settings, mode))
        .with_context(|| format!("the agent of chat {}", chat.name))
}

/// The modes the agents and last sessions `saved` keeps started in; one
/// consumed since it was listed has none.
fn recorded_modes(saved: &Saved) -> Result<Vec<String>> {
    let mut modes = Vec::new();
    for (_, path) in &saved.agents {
        modes.extend(Record::load(path)?.map(|record| record.mode));
    }
    for (_, path) in &saved.sessions {
        modes.extend(LastSession::load(path)?.map(|last| last.mode));
    }
    Ok(modes)
}
