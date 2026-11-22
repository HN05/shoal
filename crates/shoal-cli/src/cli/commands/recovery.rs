//! Reconciling a workspace's record with its worktree and processes.
use anyhow::Result;

use crate::{cli::context::Context, model::Workspace};
use shoal_protocol::recovery::{ReconcileOptions, Report};
use shoal_protocol::space_names::SpaceSelector;

pub(super) async fn reconcile(
    ctx: &Context,
    workspace: &Workspace,
    options: ReconcileOptions,
) -> Result<Vec<Report>> {
    crate::client::recovery::reconcile(&ctx.paths, Some(SpaceSelector::id(&workspace.id)), options)
        .await
}
