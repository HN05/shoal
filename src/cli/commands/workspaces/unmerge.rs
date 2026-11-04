//! Undoing a workspace's local merge: the operation that undoes it, and the
//! tracked worker that moves the default branch back.
use std::path::PathBuf;

use anyhow::{Context as _, Result};

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
    execution::{self, UnmergeResult},
    model::{UnmergePlan, Unmerged},
    paths::Paths,
    protocol::operations::{OperationSpec, Step},
};

/// Undo the latest local merge of `workspace`'s branch into the default branch.
pub(in crate::cli::commands) async fn unmerge(
    ctx: &Context,
    workspace: Option<String>,
) -> Result<i32> {
    let workspace = ui::select_workspace(ctx, workspace, Fallback::CurrentDirectory).await?;
    match operations::run(ctx, OperationSpec::Unmerge { workspace }).await {
        Ok(unmerged) => {
            show_unmerged(ctx, &unmerged)?;
            Ok(0)
        }
        Err(error) => match error.downcast::<Undecided>() {
            Ok(Undecided(request)) => approvals::declined(ctx, &request),
            Err(error) => Err(error),
        },
    }
}

fn show_unmerged(ctx: &Context, unmerged: &Unmerged) -> Result<()> {
    ctx.show(unmerged, |unmerged| {
        let (branch, default, commit) =
            (&unmerged.branch, &unmerged.default_branch, &unmerged.commit);
        match unmerged.replayed {
            0 => outln!("Moved {default} back to {commit}, before {branch} merged"),
            replayed => outln!(
                "Took {branch} out of {default} and replayed {replayed} later commit{} ({commit})",
                if replayed == 1 { "" } else { "s" }
            ),
        }
        if unmerged.branch_recreated {
            outln!("Recreated branch {branch} at {}", unmerged.source);
        }
    })
}

/// Undo `workspace`'s merge in an operation's worker, as a tracked
/// execution whose worker writes what it undid to a file.
pub(in crate::cli::commands) async fn unmerge_branch(
    ctx: &Context,
    workspace: String,
    reporter: &Reporter<'_>,
) -> Result<Unmerged> {
    reporter.step(Step::UnmergeStarted).await?;
    run_unmerge(&ctx.paths, workspace).await
}

/// Undo `workspace`'s merge as a tracked execution, reading what its worker
/// undid, or why it could not, from the file the worker writes.
async fn run_unmerge(paths: &Paths, workspace: String) -> Result<Unmerged> {
    let result = tempfile::NamedTempFile::new().context("create the unmerge result file")?;
    let code = execution::unmerge(paths, workspace, result.path().to_owned()).await?;
    let written = std::fs::read(result.path()).context("read the unmerge result")?;
    let unmerged: UnmergeResult = match written.is_empty() {
        true => Err(format!("unmerge exited with status {code}")),
        false => serde_json::from_slice(&written).context("read the unmerge result")?,
    };
    unmerged.map_err(anyhow::Error::msg)
}

/// Undo the planned merge inside the unmerge execution, writing what it
/// undid, or why it could not, to `result`.
pub(in crate::cli::commands) async fn unmerge_worker(
    ctx: &Context,
    plan: String,
    result: PathBuf,
) -> Result<i32> {
    let unmerged: UnmergeResult = finish(ctx, plan)
        .await
        .map_err(|error| format!("{error:#}"));
    std::fs::write(&result, serde_json::to_vec(&unmerged)?)
        .with_context(|| format!("write {}", result.display()))?;
    Ok(if unmerged.is_ok() { 0 } else { 1 })
}

async fn finish(ctx: &Context, plan: String) -> Result<Unmerged> {
    anyhow::ensure!(
        env::is_scoped(),
        "unmerge worker requires a tracked execution"
    );
    crate::client::executions::check_unmerge(&ctx.paths).await?;
    let plan: UnmergePlan = serde_json::from_str(&plan)?;
    anyhow::ensure!(
        std::env::var(env::WORKSPACE_ID)? == plan.workspace.id,
        "unmerge worker requires its authorized workspace"
    );
    crate::git::unmerge::finish_unmerge(plan).await
}
