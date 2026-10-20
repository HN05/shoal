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

    // An expired record is not restorable even before the sweep deletes it.
    let id = workspace.id.clone();
    let expires_at = removed.expires_at as i64;
    manager
        .store
        .run(move |db| {
            db.execute(
                "UPDATE removed_workspaces SET expires_at=unixepoch() WHERE workspace_id=?1",
                [&id],
            )?;
            Ok(())
        })
        .await
        .unwrap();
    assert!(manager.removed_workspaces().await.unwrap().is_empty());
    let error = manager.restore_workspace("topic").await.unwrap_err();
    assert!(format!("{error:#}").contains("no restorable workspace topic"));
    let id = workspace.id.clone();
    manager
        .store
        .run(move |db| {
            db.execute(
                "UPDATE removed_workspaces SET expires_at=?2 WHERE workspace_id=?1",
                rusqlite::params![id, expires_at],
            )?;
            Ok(())
        })
        .await
        .unwrap();

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

#[tokio::test]
async fn restore_reopens_the_branch_with_its_files_and_issue() {
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
    fs::write(workspace.path.join("work"), "edited\n").unwrap();
    fs::write(workspace.path.join("scratch"), "notes\n").unwrap();
    fs::remove_file(workspace.path.join("tracked")).unwrap();
    let id = workspace.id.clone();
    manager
        .store
        .run(move |db| {
            db.execute(
                "INSERT INTO workspace_issue(workspace_id,url,title) VALUES (?1,'https://github.com/acme/repo/issues/1','Fix it')",
                [id],
            )?;
            Ok(())
        })
        .await
        .unwrap();
    let status = git(&workspace.path, &["status", "--porcelain"]);
    manager
        .remove_workspace(
            &workspace.id,
            BranchChoice::DeleteBranch,
            InspectionPolicy::IncludeDirectoryProcesses,
        )
        .await
        .unwrap();
    let error = manager.restore_workspace("missing").await.unwrap_err();
    assert!(format!("{error:#}").contains("no restorable workspace missing"));
    assert_eq!(manager.removed_workspaces().await.unwrap().len(), 1);

    git(
        &checkout,
        &[
            "remote",
            "add",
            "origin",
            "https://github.com/acme/repo.git",
        ],
    );
    let restored = manager.restore_workspace("topic").await.unwrap();
    assert!(restored.warnings.is_empty(), "{:?}", restored.warnings);
    let path = &restored.opened.workspace.path;
    assert!(!restored.opened.reused);
    assert_eq!(restored.opened.workspace.branch, "topic");
    assert_eq!(git(path, &["rev-parse", "HEAD"]), head);
    assert_eq!(git(path, &["status", "--porcelain"]), status);
    assert_eq!(fs::read_to_string(path.join("scratch")).unwrap(), "notes\n");
    let workspace = manager.workspace("topic").await.unwrap();
    assert_eq!(
        workspace.links.issue.as_deref(),
        Some("https://github.com/acme/repo/issues/1")
    );
    assert!(manager.removed_workspaces().await.unwrap().is_empty());
    assert!(
        git::restore::workspace_ids(&checkout)
            .await
            .unwrap()
            .is_empty()
    );
}

#[tokio::test]
async fn restore_refuses_a_branch_that_moved_away_or_is_in_use() {
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
    manager
        .remove_workspace(
            &workspace.id,
            BranchChoice::KeepBranch,
            InspectionPolicy::IncludeDirectoryProcesses,
        )
        .await
        .unwrap();
    // The kept branch was reset elsewhere, losing the removed commit.
    git(&checkout, &["branch", "-f", "topic", "main"]);
    let error = manager.restore_workspace("topic").await.unwrap_err();
    assert!(
        format!("{error:#}").contains("no longer contains the removed HEAD"),
        "{error:#}"
    );
    git(&checkout, &["branch", "-D", "topic"]);
    manager
        .create_workspace(&repo.id, "topic".into(), None, None, None)
        .await
        .unwrap();
    let error = manager.restore_workspace("topic").await.unwrap_err();
    assert!(
        format!("{error:#}").contains("already uses branch topic"),
        "{error:#}"
    );
    assert_eq!(manager.removed_workspaces().await.unwrap().len(), 1);
}

#[tokio::test]
async fn a_removal_interrupted_after_deleting_files_stays_restorable() {
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
    fs::write(workspace.path.join("scratch"), "notes\n").unwrap();
    let check = manager
        .check_removal(&workspace.id, InspectionPolicy::GitOnly)
        .await
        .unwrap();
    manager
        .save_restore_point(&repo, &workspace, &check, EventCause::Idle)
        .await
        .unwrap();
    assert!(
        manager.removed_workspaces().await.unwrap().is_empty(),
        "the workspace still exists"
    );
    // The files go, then the daemon stops before the removal commits; the
    // next sweeps forget the deleted worktree and keep its restore point.
    fs::remove_dir_all(&workspace.path).unwrap();
    crate::daemon::cleanup::sweep(&manager, &mut Default::default())
        .await
        .unwrap();
    assert!(manager.workspace(&workspace.id).await.is_err());
    manager.expire_restore_points().await.unwrap();
    assert_eq!(manager.removed_workspaces().await.unwrap().len(), 1);

    let restored = manager.restore_workspace("topic").await.unwrap();
    let path = &restored.opened.workspace.path;
    assert_eq!(fs::read_to_string(path.join("scratch")).unwrap(), "notes\n");
    assert_eq!(fs::read_to_string(path.join("work")).unwrap(), "work\n");
}

#[tokio::test]
async fn a_failed_removal_keeps_its_restore_point() {
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
    fs::write(workspace.path.join("scratch"), "notes\n").unwrap();
    // An unreadable Worktrunk config fails the removal after the restore
    // point is saved.
    let config = manager.paths.worktrunk_config();
    let valid = fs::read_to_string(&config).unwrap();
    fs::write(&config, "not [valid toml").unwrap();
    let removed = manager
        .remove_workspace(
            &workspace.id,
            BranchChoice::DeleteBranch,
            InspectionPolicy::IncludeDirectoryProcesses,
        )
        .await;
    fs::write(&config, valid).unwrap();
    assert!(removed.is_err());
    let record = record(&manager, &workspace.id).await.unwrap();
    assert_eq!(
        git(
            &checkout,
            &["show", &format!("{}:scratch", record.snapshot)]
        ),
        "notes\n"
    );
    assert_eq!(
        git::restore::workspace_ids(&checkout).await.unwrap(),
        std::slice::from_ref(&workspace.id)
    );
    assert!(
        manager.removed_workspaces().await.unwrap().is_empty(),
        "the workspace still exists"
    );
}

#[tokio::test]
async fn restore_keeps_the_recorded_base_and_its_stack() {
    let (root, manager) = manager().await;
    let checkout = repository(root.path(), "repo");
    let repo = manager
        .register_repository(checkout.to_str().unwrap().into(), None, None)
        .await
        .unwrap();
    manager
        .create_workspace(&repo.id, "lower".into(), None, None, None)
        .await
        .unwrap();
    let upper = manager
        .create_workspace(&repo.id, "upper".into(), Some("lower".into()), None, None)
        .await
        .unwrap();
    assert_eq!(upper.base_ref.as_deref(), Some("refs/heads/lower"));
    manager
        .remove_workspace(
            &upper.id,
            BranchChoice::KeepBranch,
            InspectionPolicy::IncludeDirectoryProcesses,
        )
        .await
        .unwrap();
    manager.restore_workspace("upper").await.unwrap();
    let restored = manager.workspace("upper").await.unwrap();
    assert_eq!(restored.base_ref.as_deref(), Some("refs/heads/lower"));
    assert_eq!(
        restored.base_workspace.map(|base| base.name).as_deref(),
        Some("lower")
    );
}

#[tokio::test]
async fn a_retried_removal_keeps_the_earlier_snapshot_reachable() {
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
    let save = async || {
        let check = manager
            .check_removal(&workspace.id, InspectionPolicy::GitOnly)
            .await
            .unwrap();
        manager
            .save_restore_point(&repo, &workspace, &check, EventCause::Manual)
            .await
            .unwrap()
    };
    fs::write(workspace.path.join("first"), "first\n").unwrap();
    let first = save().await;
    // The failed attempt deleted the file; the retry finds the rest.
    fs::remove_file(workspace.path.join("first")).unwrap();
    fs::write(workspace.path.join("second"), "second\n").unwrap();
    let second = save().await;
    assert_eq!(
        git(&checkout, &["rev-parse", &format!("{}^2", second.snapshot)]).trim(),
        first.snapshot
    );
    assert_eq!(
        git(
            &checkout,
            &["show", &format!("{}^2:first", second.snapshot)]
        ),
        "first\n"
    );
    // A clean retry still keeps it.
    fs::remove_file(workspace.path.join("second")).unwrap();
    let third = save().await;
    assert_eq!(
        git(&checkout, &["rev-parse", &format!("{}^2", third.snapshot)]).trim(),
        second.snapshot
    );
}
