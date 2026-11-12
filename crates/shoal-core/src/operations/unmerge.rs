//! Undoing a workspace's local merge as a tracked execution, whose worker
//! writes what it undid to a file.
use anyhow::{Context as _, Result};

use super::Reporter;
use crate::{
    client::space_names::SpaceSelector,
    execution::{self, UnmergeResult},
    model::Unmerged,
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
