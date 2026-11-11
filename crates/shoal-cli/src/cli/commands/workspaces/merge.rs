//! Merging a workspace's branch: the operation that merges it, and the
//! tracked worker that runs the merge.
use std::path::PathBuf;

use anyhow::{Context as _, Result};

use crate::client::space_names::SpaceSelector;
use crate::{
    cli::{
        commands::{
            approvals,
            operations::{self, Reporter},
        },
        context::Context,
        ui::{self, Fallback},
    },
    client::approvals::Undecided,
    env,
    execution::{self, MergeResult},
    model::{MergeConfirmation, MergeOptions, MergePlan, MergedBranch},
    paths::Paths,
    protocol::{
        ConfigTarget,
        operations::{OperationSpec, Step},
    },
};

/// `push` overrides `[merge] push` when given, and `yes` skips the
/// confirmation a merge asks for when it pushes or brings the default branch
/// sensitive files it changed too.
pub(in crate::cli::commands) async fn merge(
    ctx: &Context,
    workspace: Option<SpaceSelector>,
    push: Option<bool>,
    yes: bool,
) -> Result<i32> {
    let workspace = ui::select_workspace(ctx, workspace, Fallback::CurrentDirectory).await?;
    let settings =
        crate::client::workspaces::settings(&ctx.paths, ConfigTarget::Workspace(workspace.clone()))
            .await?;
    let push = push.unwrap_or(settings.merge.push);
    let confirmed = match yes {
        true => None,
        // An agent's merge asks the user through its approval instead, which
        // names the files the agent's --yes passed over.
        false => {
            let pushing = push && settings.merge.confirm && !env::is_scoped();
            confirm_merge(ctx, &workspace, push, pushing).await?
        }
    };
    let options = MergeOptions { push, confirmed };
    let spec = OperationSpec::Merge {
        workspace: workspace.clone(),
        options,
    };
    match operations::run(ctx, spec).await {
        Ok(merged) => {
            show_merged(ctx, &merged)?;
            Ok(0)
        }
        Err(error) => match error.downcast::<Undecided>() {
            Ok(Undecided(request)) => approvals::declined(ctx, &request),
            Err(error) => Err(error),
        },
    }
}

/// Ask before merging `workspace`, showing what the merge does, when it
/// pushes and `pushing` asks for that, or when it brings the default branch
/// sensitive files it changed too; return what the user confirmed, if
/// asked. The daemon checks the merge first, so a merge it refuses or the
/// user declines changes nothing.
async fn confirm_merge(
    ctx: &Context,
    workspace: &SpaceSelector,
    push: bool,
    pushing: bool,
) -> Result<Option<MergeConfirmation>> {
    let preview =
        crate::client::workspaces::preview_merge(&ctx.paths, workspace.clone(), push).await?;
    if !pushing && preview.warning.is_none() {
        return Ok(None);
    }
    anyhow::ensure!(
        ui::confirm(ctx, &preview.description(), "--yes")?,
        "merge canceled"
    );
    Ok(Some(preview.confirmation()))
}

fn show_merged(ctx: &Context, merged: &MergedBranch) -> Result<()> {
    ctx.show(merged, |merged| {
        let refresh = &merged.default_refresh;
        if refresh.updated {
            outln!(
                "Updated {} from its upstream ({}..{})",
                refresh.branch,
                refresh.previous_commit,
                refresh.commit
            );
        }
        let (branch, default, commit) = (&merged.branch, &merged.default_branch, &merged.commit);
        if !merged.updated {
            outln!("{default} already contains {branch} ({commit})");
        } else if merged.fast_forward {
            outln!("Fast-forwarded {default} to {branch} ({commit})");
        } else {
            outln!("Merged {branch} into {default} ({commit})");
        }
        if let Some(remote) = &merged.pushed_to {
            outln!("Pushed {default} to {remote}");
        }
    })
}

/// Merge `workspace` as `options` say in an operation's worker, as a
/// tracked execution whose worker writes what it merged to a file.
pub(in crate::cli::commands) async fn merge_branch(
    ctx: &Context,
    workspace: SpaceSelector,
    options: MergeOptions,
    reporter: &Reporter<'_>,
) -> Result<MergedBranch> {
    reporter.step(Step::MergeStarted).await?;
    run_merge(&ctx.paths, workspace, options).await
}

/// Merge `workspace` as a tracked execution, reading what its worker merged,
/// or why it could not, from the file the worker writes.
async fn run_merge(
    paths: &Paths,
    workspace: SpaceSelector,
    options: MergeOptions,
) -> Result<MergedBranch> {
    let result = tempfile::NamedTempFile::new().context("create the merge result file")?;
    let code = execution::merge(paths, workspace, options, result.path().to_owned()).await?;
    let written = std::fs::read(result.path()).context("read the merge result")?;
    let merged: MergeResult = match written.is_empty() {
        true => Err(format!("merge exited with status {code}")),
        false => serde_json::from_slice(&written).context("read the merge result")?,
    };
    merged.map_err(anyhow::Error::msg)
}

/// Merge the planned branch inside the merge execution, writing what it
/// merged, or why it could not, to `result`.
pub(in crate::cli::commands) async fn merge_worker(
    ctx: &Context,
    plan: String,
    result: PathBuf,
) -> Result<i32> {
    let merged: MergeResult = finish(ctx, plan)
        .await
        .map_err(|error| format!("{error:#}"));
    std::fs::write(&result, serde_json::to_vec(&merged)?)
        .with_context(|| format!("write {}", result.display()))?;
    Ok(if merged.is_ok() { 0 } else { 1 })
}

async fn finish(ctx: &Context, plan: String) -> Result<MergedBranch> {
    anyhow::ensure!(
        env::is_scoped(),
        "merge worker requires a tracked execution"
    );
    crate::client::executions::check_merge(&ctx.paths).await?;
    let plan: MergePlan = serde_json::from_str(&plan)?;
    anyhow::ensure!(
        std::env::var(env::WORKSPACE_ID)? == plan.workspace.id,
        "merge worker requires its authorized workspace"
    );
    crate::git::repo::finish_merge(plan).await
}
