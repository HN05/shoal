//! The operations a request names, as its caller may reach them, and the
//! reports of their workers, which the agents and spaces act on.
use std::sync::Arc;

use anyhow::{Result, ensure};
use shoal_daemon_base::operations::Entry;

use crate::daemon::{manager::Manager, scope::Caller};
use crate::protocol::operations::{OperationKind, OperationRef, Report, Step};
use crate::refusal::{Reason, Refusal};

/// Operation `id`, refusing a scoped caller one outside its tree that its
/// own space did not start.
pub(in crate::daemon) async fn operation(
    manager: &Manager,
    id: &str,
    caller: Option<&Caller>,
) -> Result<Arc<Entry>> {
    let entry = manager
        .operations
        .get(id)
        .ok_or_else(|| Refusal::new(Reason::NotFound, format!("operation {id} does not exist")))?;
    if let Some(caller) = caller {
        ensure!(
            follows(manager, caller, &entry).await?,
            "scoped processes can only follow operations their space started or that work in their own space or one below it"
        );
    }
    Ok(entry)
}

/// Whether `caller` follows `entry`: one its own space started, such as a
/// move after its space left this machine, or one in a space its scope
/// reaches.
async fn follows(manager: &Manager, caller: &Caller, entry: &Entry) -> Result<bool> {
    Ok(entry.started_by(caller)
        || manager
            .coordination
            .reaches(manager, caller, &entry.workspace)
            .await?)
}

/// The operation `named` names, refusing a scoped caller one outside its
/// tree, and one that works in another space than the one it names. One
/// this machine does not run, named with a space it has none of, is
/// refused for that space's machine to answer; one it runs is its own
/// whatever became of its space, as a move's once the space left.
pub(in crate::daemon) async fn named_operation(
    manager: &Manager,
    named: &OperationRef,
    caller: Option<&Caller>,
) -> Result<Arc<Entry>> {
    let Some(space) = &named.space else {
        return operation(manager, &named.id, caller).await;
    };
    if manager.operations.get(&named.id).is_none() {
        manager.spaces.space(space).await?;
    }
    let entry = operation(manager, &named.id, caller).await?;
    let works_in = match space.given_id() {
        Some(id) => *id == entry.workspace,
        None => manager.spaces.space(space).await?.id == entry.workspace,
    };
    ensure!(
        works_in,
        Refusal::new(
            Reason::NotFound,
            format!("operation {} does not work in space {space}", named.id)
        )
    );
    Ok(entry)
}

/// Record the worker's `report` of operation `id`.
pub(in crate::daemon) async fn report(
    manager: &Manager,
    id: &str,
    report: Report,
    caller: Option<&Caller>,
) -> Result<()> {
    let entry = operation(manager, id, caller).await?;
    match report {
        Report::Step(step) => {
            if step == Step::PostSetupEnded {
                (manager.agents)
                    .release_setup_waits(manager, &entry.workspace)
                    .await?;
            }
            entry.report_step(step);
        }
        Report::Ended(ended) => {
            (manager.spaces)
                .keep_preparation_end(&entry.workspace, id, &ended)
                .await;
            // Kept first, so what reads the end while it is told finds it.
            entry.report_end(ended.clone());
            match entry.kind {
                OperationKind::FinishMove => {
                    manager.spaces.tell_finished_move(manager, &ended).await
                }
                OperationKind::Move => {
                    (manager.spaces)
                        .tell_move_after_turn(manager, id, &ended)
                        .await
                }
                _ => {}
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests;
