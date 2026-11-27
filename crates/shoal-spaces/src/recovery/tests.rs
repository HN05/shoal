use crate::recovery::*;
use crate::stand_in::Fixture;
use crate::state::ExecutionState;
use crate::state::WorkspaceState;
use anyhow::Result;
use shoal_core::test_support::{git, repository};
use shoal_store::test_support::seeds;

use anyhow::Context;
use shoal_protocol::space_names::SpaceSelector;
use std::{sync::Arc, time::Duration};
use tokio::time::timeout;

#[tokio::test]
async fn recovery_reads_only_its_executions_without_inspection_details() -> Result<()> {
    for stop in [false, true] {
        let manager = Fixture::new().await;
        let path = repository(manager.root.path(), "repo");
        let repo = manager
            .register_repository(path.to_string_lossy().into_owned().into(), None, None)
            .await?;
        let workspace = manager
            .create_workspace(&repo.id, "recovery".into(), None, None, None)
            .await?;
        let other = manager
            .create_workspace(&repo.id, "other".into(), None, None, None)
            .await?;
        let id = workspace.id.clone();
        manager
            .store
            .run(move |db| {
                db.execute(
                    "INSERT INTO space_completion(space_id,record) VALUES (?1,'invalid')",
                    [&id],
                )?;
                seeds::bare_execution(db, "unknown", &id, ExecutionState::Unknown)?;
                seeds::bare_execution(db, "other", &other.id, ExecutionState::Running)?;
                // A wrapper the daemon cannot read.
                db.execute(
                    "UPDATE executions SET wrapper='invalid' WHERE id='other'",
                    [],
                )?;
                Ok(())
            })
            .await?;
        assert!(
            manager
                .spaces
                .inspect_workspace(&*manager, &SpaceSelector::id(&workspace.id))
                .await
                .is_err()
        );

        let report = manager
            .spaces
            .reconcile(
                &*manager,
                &SpaceSelector::id(&workspace.id),
                ReconcileOptions {
                    repair: stop,
                    stop,
                    ..Default::default()
                },
            )
            .await?;
        assert_eq!(report.directory, DirectoryState::Valid);
        assert_eq!(report.executions.len(), 1);
        let execution = report.executions.first().context("execution report")?;
        assert_eq!(execution.id, "unknown");
        assert_eq!(execution.state, ExecutionState::Unknown);
        assert!(!execution.cleared);
    }
    Ok(())
}

#[tokio::test]
async fn failed_repair_restores_the_state_it_reserved_from() -> Result<()> {
    let manager = Fixture::new().await;
    let path = repository(manager.root.path(), "repo");
    let repo = manager
        .register_repository(path.to_string_lossy().into_owned().into(), None, None)
        .await?;
    let workspace = manager
        .create_workspace(&repo.id, "original".into(), None, None, None)
        .await?;
    let id = workspace.id.clone();
    manager
        .store
        .run(move |db| {
            db.execute(
                "INSERT INTO workspace_renames(workspace_id,branch) VALUES (?1,'new/topic')",
                [&id],
            )?;
            Ok(())
        })
        .await?;
    git(&workspace.path, &["branch", "-m", "unrelated"]);
    manager
        .spaces
        .set_state(&workspace.id, WorkspaceState::Failed, None)
        .await?;
    let repair = ReconcileOptions {
        repair: true,
        ..Default::default()
    };

    // Play the first repair: hold the gate with the lifecycle reserved while
    // the second repair reads the workspace and queues on that gate.
    let guard = manager.gates.lock_git(workspace.repository()).await;
    let gate = tokio::sync::OwnedMutexGuard::mutex(&guard).clone();
    manager
        .spaces
        .reserve_lifecycle(&workspace.id, WorkspaceState::Reconciling)
        .await?;
    let queued = Arc::strong_count(&gate);
    let second = {
        let manager = manager.daemon.clone();
        let id = workspace.id.clone();
        tokio::spawn(async move {
            manager
                .spaces
                .reconcile(&*manager, &SpaceSelector::id(&id), repair)
                .await
        })
    };
    timeout(Duration::from_secs(60), async {
        while Arc::strong_count(&gate) == queued {
            tokio::task::yield_now().await;
        }
    })
    .await
    .context("second repair never reached the repository gate")?;
    manager
        .spaces
        .set_state(&workspace.id, WorkspaceState::Failed, None)
        .await?;
    drop(guard);

    second
        .await?
        .expect_err("the unfinished rename cannot be repaired");
    assert_eq!(
        manager.spaces.workspace_by_id(&workspace.id).await?.state,
        WorkspaceState::Failed
    );
    Ok(())
}
