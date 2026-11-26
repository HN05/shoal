//! Merging a workspace's branch as a tracked execution, and that
//! execution's worker.
use std::path::PathBuf;

use anyhow::{Context as _, Result};

use super::Reporter;
use crate::{
    client::connection::CommandPaths,
    env,
    execution::{self, MergeResult},
    model::{MergeOptions, MergePlan, MergedBranch},
    protocol::operations::Step,
};
use shoal_protocol::space_names::SpaceSelector;

pub(super) async fn merge_branch(
    paths: &CommandPaths,
    workspace: SpaceSelector,
    options: MergeOptions,
    reporter: &Reporter<'_>,
) -> Result<MergedBranch> {
    reporter.step(Step::MergeStarted).await?;
    run_merge(paths, workspace, options).await
}

/// Merge `workspace` as a tracked execution, reading what its worker merged,
/// or why it could not, from the file the worker writes.
async fn run_merge(
    paths: &CommandPaths,
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
pub async fn merge_worker(paths: &CommandPaths, plan: String, result: PathBuf) -> Result<i32> {
    let merged: MergeResult = finish(paths, plan)
        .await
        .map_err(|error| format!("{error:#}"));
    std::fs::write(&result, serde_json::to_vec(&merged)?)
        .with_context(|| format!("write {}", result.display()))?;
    Ok(if merged.is_ok() { 0 } else { 1 })
}

async fn finish(paths: &CommandPaths, plan: String) -> Result<MergedBranch> {
    anyhow::ensure!(
        env::is_scoped(),
        "merge worker requires a tracked execution"
    );
    crate::client::executions::check_merge(paths).await?;
    let plan: MergePlan = serde_json::from_str(&plan)?;
    anyhow::ensure!(
        std::env::var(env::SPACE)? == plan.workspace.id,
        "merge worker requires its authorized workspace"
    );
    crate::git::repo::finish_merge(plan).await
}
