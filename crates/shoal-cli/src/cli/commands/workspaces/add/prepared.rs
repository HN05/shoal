//! What a start reads of a workspace's preparation: the workspace a launch
//! started its agent in, following the preparation, and the shell of a
//! workspace opened without an agent.
use anyhow::{Context as _, Result};

use super::finish::finish_add_workspace;
use crate::{
    cli::{context::Context, terminal},
    client::preparation::Preparing,
    model::Workspace,
    protocol::operations::{Prepared, StartedWorkspace},
    start_plan::ResolvedAddAgent,
};
use shoal_protocol::protocol::{Launched, Started};

/// The workspace a launch started its agent in, as a start's workspace: a
/// new or reopened one with its preparation, or an existing one whose
/// worker it queued, with nothing left to prepare.
pub(super) fn workspace_start(launched: Launched) -> Result<StartedWorkspace> {
    let Launched {
        space,
        entry,
        warnings,
        start,
    } = launched;
    let (operation, reused) = match start {
        Started::Preparing { operation, reused } => (operation, reused),
        Started::Worker => (None, true),
        Started::Queued { .. } | Started::Detached(_) => {
            anyhow::bail!("the daemon started the agent in no workspace")
        }
    };
    let workspace =
        (space.into_workspace().ok()).context("the daemon's launch is no workspace's")?;
    Ok(StartedWorkspace {
        operation,
        workspace: Box::new(workspace),
        reused,
        entry,
        warnings,
    })
}

/// End the start of `workspace`, whose preparation failed, leaving its
/// agent, if `with_agent`, waiting in the queue for a retried setup, and
/// the place opened for it hosting the workspace meanwhile.
pub(in crate::cli::commands) async fn unprepared(
    ctx: &Context,
    workspace: &Workspace,
    with_agent: bool,
) -> Result<i32> {
    let owner = crate::terminal::Owner::Workspace(&workspace.id);
    terminal::host_space(ctx, owner, with_agent, workspace.name.as_str()).await?;
    Ok(1)
}

/// Report the prepared `workspace`, opened without an agent, which `--json`
/// reports once prepared, and give it a shell.
pub(in crate::cli::commands) async fn shell_in(
    ctx: &Context,
    workspace: &Workspace,
) -> Result<i32> {
    if ctx.json {
        ctx.show(workspace, |_| ())?;
    }
    give_shell(ctx, workspace).await
}

/// Give `workspace` a shell in the place opened for it, which hosts the
/// workspace; the caller's own terminal gets its shell back.
async fn give_shell(ctx: &Context, workspace: &Workspace) -> Result<i32> {
    let owner = crate::terminal::Owner::Workspace(&workspace.id);
    terminal::host_space(ctx, owner, false, workspace.name.as_str()).await?;
    terminal::open_shell(ctx, &workspace.path).await
}

/// Follow the `operation` preparing `workspace` for its agent `agent`,
/// returning the workspace once it is prepared, or `None` when it was not.
pub(in crate::cli::commands) async fn follow_preparation(
    ctx: &Context,
    workspace: Workspace,
    operation: String,
    agent: Option<crate::agent::Agent>,
) -> Result<Option<Workspace>> {
    let started = StartedWorkspace {
        operation: Some(operation),
        workspace: Box::new(workspace),
        reused: false,
        entry: None,
        warnings: Vec::new(),
    };
    let preparing = Preparing::follow(&ctx.paths, started).await?;
    let agent = ResolvedAddAgent {
        agent,
        model: Default::default(),
    };
    // `--json` reports the workspace once the agent starts, or its waiting
    // entry instead.
    let finished = finish_add_workspace(ctx, preparing, &agent, ctx.json).await?;
    Ok(finished.map(|Prepared { workspace, .. }| *workspace))
}
