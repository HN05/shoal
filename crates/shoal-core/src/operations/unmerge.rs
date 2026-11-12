//! Undoing a workspace's local merge as a tracked execution, and that
//! execution's worker, which writes what it undid to a file.
use std::path::PathBuf;

use anyhow::{Context as _, Result};

use super::Reporter;
use crate::{
    client::space_names::SpaceSelector,
    env,
    execution::{self, UnmergeResult},
    model::{UnmergePlan, Unmerged},
    paths::CommandPaths,
    protocol::operations::Step,
};

/// Undo `workspace`'s merge.
pub(super) async fn unmerge_branch(
    paths: &CommandPaths,
    workspace: SpaceSelector,
    reporter: &Reporter<'_>,
) -> Result<Unmerged> {
    reporter.step(Step::UnmergeStarted).await?;
    run_unmerge(paths, workspace).await
}

/// Undo `workspace`'s merge as a tracked execution, reading what its worker
/// undid, or why it could not, from the file the worker writes.
async fn run_unmerge(paths: &CommandPaths, workspace: SpaceSelector) -> Result<Unmerged> {
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
pub async fn unmerge_worker(paths: &CommandPaths, plan: String, result: PathBuf) -> Result<i32> {
    let unmerged: UnmergeResult = finish(paths, plan)
        .await
        .map_err(|error| format!("{error:#}"));
    std::fs::write(&result, serde_json::to_vec(&unmerged)?)
        .with_context(|| format!("write {}", result.display()))?;
    Ok(if unmerged.is_ok() { 0 } else { 1 })
}

async fn finish(paths: &CommandPaths, plan: String) -> Result<Unmerged> {
    anyhow::ensure!(
        env::is_scoped(),
        "unmerge worker requires a tracked execution"
    );
    crate::client::executions::check_unmerge(paths).await?;
    let plan: UnmergePlan = serde_json::from_str(&plan)?;
    anyhow::ensure!(
        std::env::var(env::WORKSPACE_ID)? == plan.workspace.id,
        "unmerge worker requires its authorized workspace"
    );
    crate::git::unmerge::finish_unmerge(plan).await
}
