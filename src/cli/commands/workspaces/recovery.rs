//! Reconciling a workspace's record with its worktree and processes.
use anyhow::Result;

use crate::{
    cli::context::Context,
    client::recovery::{ReconcileOptions, Report},
    model::Workspace,
};

pub(super) async fn reconcile(
    ctx: &Context,
    workspace: &Workspace,
    options: ReconcileOptions,
) -> Result<Vec<Report>> {
    crate::client::recovery::reconcile(&ctx.paths, Some(workspace.id.clone()), options).await
}

/// The problems `reports` leave, joined for a message.
pub(super) fn issues(reports: &[Report]) -> String {
    reports
        .iter()
        .flat_map(|report| report.issues.iter().map(String::as_str))
        .collect::<Vec<_>>()
        .join("; ")
}
