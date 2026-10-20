use std::fs;

use super::*;
use crate::{
    removal::{BranchChoice, InspectionPolicy},
    test_support::{commit, git, manager, repository},
};

async fn record(manager: &Manager, id: &str) -> Option<RemovedWorkspace> {
    let id = id.to_owned();
    manager
        .store
        .run(move |db| {
            let record: Option<String> = rusqlite::OptionalExtension::optional(db.query_row(
                "SELECT record FROM removed_workspaces WHERE workspace_id=?1",
                [id],
                |row| row.get(0),
            ))?;
            Ok(record.map(|record| serde_json::from_str(&record).unwrap()))
        })
        .await
        .unwrap()
}

#[tokio::test]
async fn removal_keeps_a_restore_point_until_it_expires() {
    let (root, manager) = manager().await;
    let checkout = repository(root.path(), "repo");
    let repo = manager
        .register_repository(checkout.to_str().unwrap().into(), None, None)
        .await
        .unwrap();
    let workspace = manager
        .create_workspace(&repo.id, "topic".into(), None, None, None)
        .await
        .unwrap();
    fs::write(workspace.path.join("work"), "work\n").unwrap();
    commit(&workspace.path, "work");
    let head = git(&workspace.path, &["rev-parse", "HEAD"]);
    fs::write(workspace.path.join("scratch"), "notes\n").unwrap();
    let id = workspace.id.clone();
    manager
        .store
        .run(move |db| {
            db.execute(
                "INSERT INTO workspace_issue(workspace_id,url) VALUES (?1,'https://forge/issue/1')",
                [id],
            )?;
            Ok(())
        })
        .await
        .unwrap();

    manager
        .remove_workspace(
            &workspace.id,
            BranchChoice::DeleteBranch,
            InspectionPolicy::IncludeDirectoryProcesses,
        )
        .await
        .unwrap();
    assert_eq!(git(&checkout, &["branch", "--list", "topic"]), "");
    let removed = record(&manager, &workspace.id).await.unwrap();
    assert_eq!(removed.name, "topic");
    assert_eq!(removed.branch.as_deref(), Some("topic"));
    assert_eq!(removed.head, head.trim());
    assert!(!removed.files_omitted);
    assert_eq!(removed.cause, EventCause::Manual);
    assert_eq!(
        removed.links.issue.as_deref(),
        Some("https://forge/issue/1")
    );
    assert_eq!(removed.expires_at - removed.removed_at, 30 * 24 * 3600);
    let reference = git::restore::reference(&workspace.id);
    assert_eq!(
        git(&checkout, &["rev-parse", &reference]).trim(),
        removed.snapshot
    );
    assert_eq!(
        git(
            &checkout,
            &["show", &format!("{}:scratch", removed.snapshot)]
        ),
        "notes\n"
    );

    // Expiry deletes the record and its ref, and refs nothing owns.
    git(
        &checkout,
        &["update-ref", "refs/shoal/removed/unknown", "HEAD"],
    );
    manager.expire_restore_points().await.unwrap();
    assert!(record(&manager, &workspace.id).await.is_some());
    assert_eq!(
        git::restore::workspace_ids(&checkout).await.unwrap(),
        std::slice::from_ref(&workspace.id)
    );
    let id = workspace.id.clone();
    manager
        .store
        .run(move |db| {
            db.execute(
                "UPDATE removed_workspaces SET expires_at=0 WHERE workspace_id=?1",
                [id],
            )?;
            Ok(())
        })
        .await
        .unwrap();
    manager.expire_restore_points().await.unwrap();
    assert!(record(&manager, &workspace.id).await.is_none());
    assert!(
        git::restore::workspace_ids(&checkout)
            .await
            .unwrap()
            .is_empty()
    );
}
