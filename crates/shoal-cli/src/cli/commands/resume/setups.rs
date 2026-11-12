//! A workspace whose setup failed, or never started, resumes by retrying its
//! setup, after which the agent waiting in the queue for it starts.
use anyhow::{Result, ensure};

use super::targets::Selected;
use crate::{
    cli::{commands::workspaces, context::Context, output::Style},
    config::permissions::PermissionChange,
    model::Workspace,
};

/// Retry the setup of the selected workspaces whose setup failed, when the
/// selection names only such workspaces, and report them together; `None`
/// when it names none. Other spaces beside them, a `change` to the
/// permissions, which the agents waiting for those setups cannot take, and
/// `placing` options, since the queue starts those agents as it admits them,
/// are refused before any setup runs.
pub(super) async fn retry_failed(
    ctx: &Context,
    selected: &mut Selected,
    change: Option<&PermissionChange>,
    placing: bool,
) -> Result<Option<i32>> {
    let failed = take_failed(ctx, selected).await?;
    let Some(first) = failed.first() else {
        return Ok(None);
    };
    ensure!(
        selected.is_empty(),
        "the setup of {} failed; resume the workspaces whose setup failed apart from other spaces",
        first.name
    );
    ensure!(
        change.is_none(),
        "the setup of {} failed, and the agent waiting for it keeps the permissions it was started with; resume it without --permission",
        first.name
    );
    ensure!(
        !placing,
        "the setup of {} failed, and the agent waiting for it starts from the queue as it admits it; resume it without --now or --focus",
        first.name
    );
    let mut set_up = Vec::new();
    let mut errors = Vec::new();
    for workspace in &failed {
        match retry(ctx, workspace).await {
            Ok(Some(workspace)) => set_up.push(workspace),
            Ok(None) => {}
            Err(error) => errors.push(error),
        }
    }
    report(ctx, set_up, errors, failed.len())
}

/// Run `workspace`'s setup and its post-setup hook again; `None` when the
/// user kept or deleted the workspace after it failed again.
async fn retry(ctx: &Context, workspace: &Workspace) -> Result<Option<Workspace>> {
    match workspaces::setup_workspace(ctx, workspace, true).await? {
        Some(set_up) => set_up.finish(ctx).await.map(Some),
        None => Ok(None),
    }
}

/// Report the workspaces `set_up` of the `retried` ones in one result, and
/// each failure; one that failed alone is the command's error.
fn report(
    ctx: &Context,
    set_up: Vec<Workspace>,
    mut errors: Vec<anyhow::Error>,
    retried: usize,
) -> Result<Option<i32>> {
    if set_up.is_empty() && errors.len() == 1 {
        return Err(errors.remove(0));
    }
    for error in &errors {
        eprintln!("warning: {error:#}");
    }
    let names: Vec<_> = set_up
        .iter()
        .map(|workspace| workspace.name.to_string())
        .collect();
    if !names.is_empty() {
        let text = format!("Set up {}", names.join(", "));
        ctx.emit_styled(Style::Success, &text, &set_up)?;
    }
    Ok(Some(i32::from(set_up.len() < retried)))
}

/// The selected workspaces whose setup failed or never started, which leave
/// the selection.
async fn take_failed(ctx: &Context, selected: &mut Selected) -> Result<Vec<Workspace>> {
    let Some(selectors) = selected.workspaces.take() else {
        return Ok(Vec::new());
    };
    let mut failed = Vec::new();
    let mut rest = Vec::new();
    for selector in selectors {
        let inspection = crate::client::workspaces::inspect(&ctx.paths, selector.clone()).await?;
        match workspaces::awaits_setup(&inspection) {
            true => failed.push(inspection.workspace),
            false => rest.push(selector),
        }
    }
    selected.workspaces = Some(rest);
    Ok(failed)
}
