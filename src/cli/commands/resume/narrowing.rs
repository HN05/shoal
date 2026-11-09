//! `shoal resume --permission`: a narrowing that replaces the one the
//! restored agents and chats recorded. Each one's mode bounds it, which is
//! checked for every selected space before any narrowing is replaced.
use anyhow::{Context as _, Result};

use super::targets::Selected;
use crate::client::space_names::SpaceSelector;
use crate::execution::recovery::{LastSessionExt, RecordExt};
use crate::{
    cli::context::Context,
    config::permissions::{PermissionsConfig, SpaceMode},
    execution::recovery::{LastSession, Record, Saved},
    model::{SpaceKind, Workspace},
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
        let workspace =
            crate::client::workspaces::inspect(&ctx.paths, SpaceSelector::parse(selector))
                .await?
                .workspace;
        let mut saved = Saved::load(&ctx.paths, &workspace.id)?;
        if let Some(execution) = execution {
            saved.retain_execution(execution, false);
        }
        ensure_fits(ctx, &workspace, &saved, &narrowing).await?;
        stopped.push((workspace, saved));
    }
    for chat in &selected.chats {
        super::chats::ensure_fits(ctx, chat, &narrowing).await?;
    }
    for (workspace, saved) in stopped {
        for execution in (saved.agents.into_iter().chain(saved.sessions)).map(|(id, _)| id) {
            let selector = SpaceSelector::id(&workspace.id);
            crate::client::assignments::replace_stopped_narrowing(
                &ctx.paths, selector, execution, narrowing,
            )
            .await?;
        }
    }
    Ok(())
}

/// Refuse `narrowing` where it is wider than the permissions of the mode
/// one of `workspace`'s agents and last sessions `saved` keeps recorded.
async fn ensure_fits(
    ctx: &Context,
    workspace: &Workspace,
    saved: &Saved,
    narrowing: &PermissionsConfig,
) -> Result<()> {
    let settings = crate::client::workspaces::settings(
        &ctx.paths,
        ConfigTarget::Workspace(SpaceSelector::id(&workspace.id)),
    )
    .await?;
    for mode in recorded_modes(saved)? {
        let mode = SpaceMode {
            name: &mode,
            kind: SpaceKind::Workspace,
            repository: true,
        };
        (narrowing.ensure_within_mode(&settings, mode))
            .with_context(|| format!("the agent of {}", workspace.name))?;
    }
    Ok(())
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
