//! Removing workspaces after checking what each loses.
use anyhow::{Result, ensure};
use serde_json::json;

use super::removal::{ensure_batch_removable, leaving};
use crate::{
    cli::{client::request, commands::agent_queue, context::Context, output::Style, ui},
    protocol::RemovalMethod,
    removal::{RemovalCheck, RemovalResult, UnsavedWork},
};

pub(in crate::cli::commands) async fn remove(
    ctx: &Context,
    names: Vec<String>,
    yes: bool,
) -> Result<i32> {
    if let [selector] = names.as_slice()
        && let Some(code) = agent_queue::remove(ctx, selector).await?
    {
        return Ok(code);
    }
    let workspaces = ui::select_workspaces(ctx, names).await?;
    let checks = check_removals(ctx, workspaces, yes).await?;
    if !yes {
        confirm_removals(ctx, &checks)?;
    }
    match <[RemovalCheck; 1]>::try_from(checks) {
        Ok([check]) => remove_one(ctx, check).await,
        Err(checks) => remove_each(ctx, checks).await,
    }
}

/// Check every workspace before removing any, so one that cannot be removed
/// leaves the others in place.
async fn check_removals(
    ctx: &Context,
    workspaces: Vec<String>,
    yes: bool,
) -> Result<Vec<RemovalCheck>> {
    let mut checks = Vec::new();
    let mut refusals = Vec::new();
    let several = workspaces.len() > 1;
    for workspace in workspaces {
        let method = RemovalMethod::CheckRemoval {
            workspace: workspace.clone(),
            include_changes: ctx.interactive() && !yes,
        };
        let checked = request::<RemovalCheck>(&ctx.paths, method).await;
        if !several {
            checks.push(checked?);
            continue;
        }
        match checked {
            Ok(check) => match ensure_batch_removable(ctx, &check, ("removal", "remove")).await {
                Ok(()) => checks.push(check),
                Err(error) => refusals.push(format!("{}: {error:#}", check.workspace.name)),
            },
            Err(error) => refusals.push(format!("{workspace}: {error:#}")),
        }
    }
    ensure!(
        refusals.is_empty(),
        "cannot remove these workspaces, so none was removed:\n  {}",
        refusals.join("\n  ")
    );
    Ok(checks)
}

async fn remove_one(ctx: &Context, check: RemovalCheck) -> Result<i32> {
    let result = remove_checked(ctx, &check).await?;
    let output = removal_output(&result, &check)?;
    ctx.emit_styled(
        Style::Success,
        &removal_message("Workspace removed", &result),
        &output,
    )?;
    Ok(0)
}

/// Remove each checked workspace, going on after one fails. One that a
/// removal before it took as its child is not removed again.
async fn remove_each(ctx: &Context, checks: Vec<RemovalCheck>) -> Result<i32> {
    let mut report = ui::Report::new("removed", "remove");
    let mut taken = Vec::new();
    for check in checks {
        let name = &check.workspace.name;
        if taken.contains(name) {
            continue;
        }
        match remove_checked(ctx, &check).await {
            Ok(result) => {
                let mut output = removal_output(&result, &check)?;
                output["workspace"] = json!(name);
                let message = removal_message(&format!("Removed {name}"), &result);
                taken.extend(result.children);
                report.done(ctx, &message, output);
            }
            Err(error) => report.failed(ctx, name, &error),
        }
    }
    report.finish(ctx, "")
}

/// Remove the workspace `check` inspected, deleting only the unsaved work it
/// saw: more appearing meanwhile makes the daemon refuse.
async fn remove_checked(ctx: &Context, check: &RemovalCheck) -> Result<RemovalResult> {
    let method = RemovalMethod::RemoveWorkspace {
        workspace: check.workspace.id.clone(),
        unsaved: UnsavedWork::confirming(check),
    };
    let result = leaving(
        ctx,
        &check.workspace,
        request::<RemovalResult>(&ctx.paths, method),
    )
    .await?;
    if let Some(error) = &result.hook_error {
        eprintln!("warning: {error}");
    }
    Ok(result)
}

/// What JSON reports for a removal: its result and the workspace's holds.
fn removal_output(result: &RemovalResult, check: &RemovalCheck) -> Result<serde_json::Value> {
    let mut output = serde_json::to_value(result)?;
    output["holds"] = serde_json::to_value(&check.workspace.holds)?;
    Ok(output)
}

/// Ask once before removing workspaces that would lose unsaved work or
/// holds, naming what each loses.
fn confirm_removals(ctx: &Context, checks: &[RemovalCheck]) -> Result<()> {
    let losses: Vec<(String, Vec<String>)> = checks
        .iter()
        .map(|check| (check.workspace.name.clone(), removal_losses(ctx, check)))
        .collect();
    if losses.iter().all(|(_, lines)| lines.is_empty()) {
        return Ok(());
    }
    let action = match checks {
        [check] => format!("Remove workspace: {}", check.workspace.name),
        _ => format!(
            "Remove {} workspaces: {}",
            checks.len(),
            checks
                .iter()
                .map(|check| check.workspace.name.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        ),
    };
    ensure!(
        ui::confirm_losses(ctx, &action, &losses, "--yes")?,
        "workspace removal canceled"
    );
    Ok(())
}

/// What removing the workspace `check` inspected loses: the workspaces
/// below it, its unsaved work, with the changed files on a terminal, and its
/// holds.
fn removal_losses(ctx: &Context, check: &RemovalCheck) -> Vec<String> {
    let mut lines = Vec::new();
    let unsaved = check.unsaved_work();
    if unsaved.is_empty() && check.workspace.holds.is_empty() && check.children.is_empty() {
        return lines;
    }
    if !check.children.is_empty() {
        lines.push(format!(
            "Removes the workspaces split from it first, whatever their work or holds; shoal work restore brings each back: {}",
            check
                .children
                .iter()
                .map(|child| child.name.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        ));
    }
    for work in &unsaved {
        lines.push(format!("Deletes: {work}"));
    }
    if !unsaved.is_empty() {
        lines.push(if !check.dirty || check.unsaved_files_archived {
            "shoal work restore brings them back until the checkpoint expires.".into()
        } else {
            "The checkpoint cannot keep the uncommitted files (they are too large, or include a repository or a changed submodule), so they cannot be restored.".into()
        });
    }
    if !check.workspace.holds.is_empty() {
        lines.push("Holds:".into());
        for hold in &check.workspace.holds {
            let reason = crate::cli::context::optional(hold.reason.as_deref(), |reason| {
                format!(" ({reason})")
            });
            lines.push(format!("  {}{reason}", hold.name));
        }
    }
    if ctx.interactive() && (!check.changed_files.is_empty() || check.changed_files_omitted > 0) {
        lines.push("Uncommitted changes and untracked files (Git status):".into());
        for file in &check.changed_files {
            lines.push(format!("  {file}"));
        }
        if check.changed_files_omitted > 0 {
            lines.push(format!(
                "  ... {} more entries omitted; run git status in the workspace for the full list",
                check.changed_files_omitted
            ));
        }
    }
    lines
}

/// `removed`, such as `Workspace removed`, with the Git branch the removal
/// kept and the workspaces below it it took.
fn removal_message(removed: &str, result: &RemovalResult) -> String {
    let mut message = match &result.branch {
        Some(branch) if !result.branch_outcome.is_deleted() => {
            format!("{removed}; kept Git branch {branch}")
        }
        _ => removed.into(),
    };
    if !result.children.is_empty() {
        message.push_str(&format!("; also removed {}", result.children.join(", ")));
    }
    message
}
