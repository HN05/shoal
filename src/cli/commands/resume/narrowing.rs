//! `--permission` on `shoal resume` and `shoal restart`: a change to the
//! permissions the restored or restarted agents and chats were given. A
//! scoped process gives only what it holds, which is checked for every
//! selected space before anything restarts or restores, and a restore makes
//! the change in the records it restores once nothing refuses it.
use anyhow::{Context as _, Result, ensure};

use super::targets::Selected;
use crate::client::space_names::SpaceSelector;
use crate::{
    cli::context::Context,
    config::permissions::{PermissionChange, PermissionsConfig},
    execution::recovery::{Saved, waits_in_wrapper},
    model::Workspace,
    protocol::SavedAgent,
};

/// Refuse `change` for the stopped agents and last sessions of the
/// `selected` workspaces, only `execution`'s when given, and the selected
/// chats, where this process does not hold what it gives or one's waiting
/// wrapper restores it, which keeps the permissions it launched with. It
/// writes nothing: each restore makes the change in the records it restores
/// with [`apply`] once nothing refuses it.
pub(super) async fn check(
    ctx: &Context,
    selected: &Selected,
    execution: Option<&str>,
    change: &PermissionChange,
) -> Result<()> {
    let selectors = (selected.workspaces.as_deref())
        .context("--permission changes the permissions of the spaces a resume names")?;
    for selector in selectors {
        let (workspace, saved) = saved_agents(ctx, selector, execution).await?;
        ensure!(
            !saved.agents.iter().any(|(_, path)| waits_in_wrapper(path)),
            "the agent of {} restores in the wrapper that waits for it, which keeps its permissions; resume it without --permission",
            workspace.name
        );
        let repository = Some(workspace.repository_id.as_str().into());
        ensure_held(ctx, repository, &change.set).await?;
    }
    for chat in &selected.chats {
        let repository = chat.repository_id.as_deref().map(Into::into);
        ensure_held(ctx, repository, &change.set).await?;
    }
    Ok(())
}

/// The workspace `selector` names with the agents and last sessions it
/// saved, only `execution`'s when given.
async fn saved_agents(
    ctx: &Context,
    selector: &SpaceSelector,
    execution: Option<&str>,
) -> Result<(Workspace, Saved)> {
    let workspace = crate::client::workspaces::inspect(&ctx.paths, selector.clone())
        .await?
        .workspace;
    let mut saved = Saved::load(&ctx.paths, &workspace.id)?;
    if let Some(execution) = execution {
        saved.retain_execution(execution, false);
    }
    Ok((workspace, saved))
}

/// Have the daemon make `change`, when given, to the permissions each agent
/// and last session the `stopped` workspaces keep was given, as their
/// restores begin; it changes none when it refuses one.
pub(super) async fn apply(
    ctx: &Context,
    stopped: &[(Workspace, Saved)],
    change: Option<&PermissionChange>,
) -> Result<()> {
    let agents = stopped.iter().flat_map(|(workspace, saved)| {
        (saved.agents.iter().chain(&saved.sessions)).map(|(execution, _)| SavedAgent {
            workspace: SpaceSelector::id(&workspace.id),
            execution: execution.clone(),
        })
    });
    request(ctx, agents.collect(), change).await
}

/// [`apply`] for the agent or last session `execution` of `workspace`.
pub(super) async fn apply_one(
    ctx: &Context,
    workspace: &Workspace,
    execution: &str,
    change: Option<&PermissionChange>,
) -> Result<()> {
    let agent = SavedAgent {
        workspace: SpaceSelector::id(&workspace.id),
        execution: execution.to_owned(),
    };
    request(ctx, vec![agent], change).await
}

/// Have the daemon make `change`, when given, for the stopped `agents`.
async fn request(
    ctx: &Context,
    agents: Vec<SavedAgent>,
    change: Option<&PermissionChange>,
) -> Result<()> {
    match change {
        Some(change) if !agents.is_empty() => {
            let change = change.clone();
            crate::client::assignments::change_stopped_permissions(&ctx.paths, agents, change).await
        }
        _ => Ok(()),
    }
}

/// Refuse `given` where this scoped process does not hold it for an action
/// in the registered repository `repository`; the user, without a scope,
/// gives any.
pub(in crate::cli::commands) async fn ensure_held(
    ctx: &Context,
    repository: Option<crate::repository::Selector>,
    given: &PermissionsConfig,
) -> Result<()> {
    if !crate::env::is_scoped() || given.is_empty() {
        return Ok(());
    }
    let held = crate::client::workspaces::held_permissions(&ctx.paths, repository).await?;
    given.ensure_held(&held)
}
