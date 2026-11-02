//! Merging a workspace's branch: the operation that merges it, and the
//! tracked worker that runs the merge.
use std::path::PathBuf;

use anyhow::{Context as _, Result};

use crate::{
    cli::{
        client::{self, request},
        commands::{
            approvals,
            operations::{self, Reporter},
        },
        context::Context,
        ui::{self, Fallback},
    },
    daemon::approvals::Undecided,
    env,
    execution::{self, MergeResult},
    model::{MergePlan, MergedBranch},
    paths::Paths,
    protocol::{
        ConfigTarget, ExecutionMethod, WorkspaceMethod,
        operations::{OperationSpec, Step},
    },
};

/// `push` overrides `[merge] push` when given. Unless `confirmed`, the user
/// confirms a merge the daemon warns about.
pub(in crate::cli::commands) async fn merge(
    ctx: &Context,
    workspace: Option<String>,
    push: Option<bool>,
    confirmed: bool,
) -> Result<i32> {
    let workspace = ui::select_workspace(ctx, workspace, Fallback::CurrentDirectory).await?;
    if !confirmed {
        let warning = WorkspaceMethod::BranchMergeWarning {
            workspace: workspace.clone(),
        };
        crate::cli::commands::merge_warning::confirm(ctx, request(&ctx.paths, warning).await?)?;
    }
    let spec = OperationSpec::Merge { workspace, push };
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

/// Merge `workspace` in an operation's worker, as a tracked execution whose
/// worker writes what it merged to a file; `push` overrides `[merge] push`.
pub(in crate::cli::commands) async fn merge_branch(
    ctx: &Context,
    workspace: String,
    push: Option<bool>,
    reporter: &Reporter<'_>,
) -> Result<MergedBranch> {
    let push = match push {
        Some(push) => push,
        None => {
            client::settings(&ctx.paths, ConfigTarget::Workspace(workspace.clone()))
                .await?
                .merge
                .push
        }
    };
    reporter.step(Step::MergeStarted).await?;
    run_merge(&ctx.paths, workspace, push).await
}

/// Merge `workspace` as a tracked execution, reading what its worker merged,
/// or why it could not, from the file the worker writes.
async fn run_merge(paths: &Paths, workspace: String, push: bool) -> Result<MergedBranch> {
    let result = tempfile::NamedTempFile::new().context("create the merge result file")?;
    let code = execution::merge(paths, workspace, push, result.path().to_owned()).await?;
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
    request::<()>(&ctx.paths, ExecutionMethod::CheckMerge).await?;
    let plan: MergePlan = serde_json::from_str(&plan)?;
    anyhow::ensure!(
        std::env::var(env::WORKSPACE_ID)? == plan.workspace.id,
        "merge worker requires its authorized workspace"
    );
    crate::git::repo::finish_merge(plan).await
}
