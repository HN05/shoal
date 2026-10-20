use super::*;
use crate::test_support::{manager, repository};
use anyhow::Context;

#[tokio::test]
async fn recovery_reads_only_its_executions_without_inspection_details() -> Result<()> {
    for stop in [false, true] {
        let (_root, manager) = manager().await;
        let path = repository(_root.path(), "repo");
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
                    "INSERT INTO workspace_completion(workspace_id,record) VALUES (?1,'invalid')",
                    [&id],
                )?;
                db.execute(
                    "INSERT INTO executions(id,workspace_id,state) VALUES ('unknown',?1,'unknown')",
                    [&id],
                )?;
                db.execute(
                    "INSERT INTO executions(id,workspace_id,state,wrapper)
                     VALUES ('other',?1,'running','invalid')",
                    [&other.id],
                )?;
                Ok(())
            })
            .await?;
        assert!(manager.inspect_workspace(&workspace.id).await.is_err());

        let report = manager
            .reconcile(
                &workspace.id,
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
        assert!(
            report
                .issues
                .iter()
                .any(|issue| issue == "Execution unknown remains unresolved")
        );
    }
    Ok(())
}
