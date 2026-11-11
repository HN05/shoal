//! A workspace whose setup failed resumes by retrying its setup, after which
//! the agent waiting in the queue for it starts.
use anyhow::{Result, ensure};

use super::targets::Selected;
use crate::client::space_names::SpaceSelector;
use crate::{
    cli::{commands::workspaces, context::Context},
    model::Workspace,
    state::WorkspaceState,
};

/// Retry the setup of the selected workspaces whose setup failed, taking
/// them out of `selected`. Returns the exit code once nothing else is
/// selected, `None` while something is. A `changing` --permission is
/// refused, since the agents waiting for those setups keep the permissions
/// they were started with.
pub(super) async fn retry_failed(
    ctx: &Context,
    selected: &mut Selected,
    changing: bool,
) -> Result<Option<i32>> {
    let failed = take_failed(ctx, selected).await?;
    let Some(first) = failed.first() else {
        return Ok(None);
    };
    ensure!(
        !changing,
        "the setup of {} failed, and the agent waiting for it keeps the permissions it was started with; resume it without --permission",
        first.name
    );
    let mut code = 0;
    for workspace in failed {
        let selector = SpaceSelector::id(&workspace.id);
        code = code.max(workspaces::setup(ctx, Some(selector)).await?);
    }
    Ok(selected.is_empty().then_some(code))
}

/// The selected workspaces whose setup failed, which leave the selection.
async fn take_failed(ctx: &Context, selected: &mut Selected) -> Result<Vec<Workspace>> {
    let Some(selectors) = selected.workspaces.take() else {
        return Ok(Vec::new());
    };
    let mut failed = Vec::new();
    let mut rest = Vec::new();
    for selector in selectors {
        let inspection = crate::client::workspaces::inspect(&ctx.paths, selector.clone()).await?;
        match inspection.workspace.state {
            WorkspaceState::Failed => failed.push(inspection.workspace),
            _ => rest.push(selector),
        }
    }
    selected.workspaces = Some(rest);
    Ok(failed)
}
