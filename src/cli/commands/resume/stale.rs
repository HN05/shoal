//! Resuming an execution a daemon restart left unknown, once the daemon has
//! rechecked that its recorded processes are gone.
use anyhow::{Context as _, Result, ensure};

use crate::client::execution_ids::ExecutionId;
use crate::client::space_names::SpaceSelector;
use crate::{
    cli::context::Context, client::recovery::ReconcileOptions, model::Inspection,
    state::ExecutionState,
};

/// A daemon restart quarantines interrupted executions as unknown. Recovery
/// records are still safe to use once the daemon has rechecked that execution's
/// recorded processes are gone, so make that repair part of resume itself. An
/// agent whose wrapper waits to restore it is stopped, not active.
pub(super) async fn reconcile(
    ctx: &Context,
    inspection: &Inspection,
    selected: Option<&ExecutionId>,
) -> Result<()> {
    let Some(selected) = selected else {
        return Ok(());
    };
    let Some(execution) = inspection
        .executions
        .iter()
        .find(|execution| execution.id == *selected)
    else {
        return Ok(());
    };
    ensure!(
        execution.state != ExecutionState::Running || execution.stopped.is_some(),
        "selected execution is active; stop it before resuming"
    );
    if execution.state != ExecutionState::Unknown {
        return Ok(());
    }
    let reports = crate::client::recovery::reconcile(
        &ctx.paths,
        Some(SpaceSelector::id(&inspection.workspace.id)),
        ReconcileOptions {
            repair: true,
            // A protection handoff is persisted only after Shoal has
            // decided to stop this agent. The pause path clears the child
            // identity before the daemon can restart, so the normal
            // launch-proof acknowledgement is unavailable even though
            // process scanning still proves that no survivor is visible.
            acknowledge_stopped: true,
            ..Default::default()
        },
    )
    .await?;
    let report = reports
        .into_iter()
        .next()
        .context("daemon returned no reconciliation report")?;
    ensure!(
        report
            .executions
            .iter()
            .find(|execution| execution.id == *selected)
            .is_some_and(|execution| execution.cleared),
        "selected execution is still unresolved; stop what remains of it with shoal stop, then resume again"
    );
    Ok(())
}
