//! The branch a start names as typed, which opens as `git switch` takes it:
//! an existing local or remote branch opens, in the workspace that already
//! has it, and any other name is a new branch under exactly that name.
use anyhow::Result;

use super::{reads::DaemonReads, target::AddTarget};
use crate::{
    cli::context::Context,
    launches::workspace,
    model::{Repository, Workspace},
    start_plan::Creation,
};

/// Settle the branch `creation` names as typed, as
/// [`workspace::settle`] does: the workspace that already has it, which the
/// start goes into instead, else the branch `creation` then opens.
pub(super) async fn settle(
    ctx: &Context,
    target: &AddTarget,
    creation: &mut Creation,
) -> Result<Option<Workspace>> {
    if creation.branch.is_none() {
        return Ok(None);
    }
    let repo = target.repository(ctx).await?;
    workspace::settle(&DaemonReads(ctx), (&target.selector, repo), creation).await
}

/// The workspace of `repo` that already has the branch `name` selects, as
/// typed or as a remote's, which a start on it goes into.
pub(in crate::cli::commands) async fn owning_workspace(
    ctx: &Context,
    repo: &Repository,
    name: &str,
) -> Result<Option<Workspace>> {
    workspace::owning_workspace(&DaemonReads(ctx), repo, name).await
}
