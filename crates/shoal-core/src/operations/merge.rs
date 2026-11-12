//! Merging a workspace's branch as a tracked execution, whose worker
//! writes what it merged to a file.
use anyhow::{Context as _, Result};

use super::Reporter;
use crate::{
    client::space_names::SpaceSelector,
    execution::{self, MergeResult},
    model::{MergeOptions, MergedBranch},
    paths::CommandPaths,
    protocol::operations::Step,
};

/// Merge `workspace` as `options` say.
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
