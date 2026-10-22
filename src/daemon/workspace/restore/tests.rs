use std::fs;

use super::*;
use crate::{
    model::CheckpointCause,
    removal::{InspectionPolicy, UnsavedWork},
    test_support::{commit, git, manager, repository},
};

/// The checkpoint refs in the repository, sorted.
pub(in crate::daemon::workspace) fn checkpoint_refs(repo: &std::path::Path) -> Vec<String> {
    let mut refs: Vec<String> = git(
        repo,
        &["for-each-ref", "--format=%(refname)", "refs/shoal/"],
    )
    .lines()
    .map(str::to_owned)
    .collect();
    refs.sort();
    refs
}

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
async fn removal_checkpoint_keeps_files_workspace_processes_write_as_they_stop() {
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
    // A process started with the workspace identity, as hooks and `shoal env`
    // exports start them, that saves its work when asked to stop.
    let ready = root.path().join("ready");
    let mut process = tokio::process::Command::new("python3")
        .args([
            "-c",
            "import signal, sys, time\n\
             def stop(*_):\n    open(sys.argv[1], 'w').write('flushed\\n'); sys.exit(0)\n\
             signal.signal(signal.SIGTERM, stop)\n\
             open(sys.argv[2], 'w').close()\n\
             while True: time.sleep(1)",
        ])
        .arg(workspace.path.join("flushed"))
        .arg(&ready)
        .env_clear()
        .env("PATH", std::env::var_os("PATH").unwrap_or_default())
        .env(crate::env::WORKSPACE_ID, &workspace.id)
        .current_dir(root.path())
        .kill_on_drop(true)
        .spawn()
        .unwrap();
    tokio::time::timeout(std::time::Duration::from_secs(60), async {
        while !ready.exists() {
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("workspace process did not start");

    manager
        .remove_workspace(&workspace.id, UnsavedWork::All, InspectionPolicy::GitOnly)
        .await
        .unwrap();

    assert!(process.wait().await.unwrap().success());
    let removed = record(&manager, &workspace.id).await.unwrap();
    assert_eq!(
        git(
            &checkout,
            &["show", &format!("{}:flushed", removed.checkpoint.commit)]
        ),
        "flushed\n"
    );
}

#[tokio::test]
async fn removal_keeps_a_checkpoint_and_record_until_they_expire() {
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
            UnsavedWork::All,
            InspectionPolicy::IncludeDirectoryProcesses,
        )
        .await
        .unwrap();
    // Its unpushed commit keeps the branch.
    assert_ne!(git(&checkout, &["branch", "--list", "topic"]), "");
    let removed = record(&manager, &workspace.id).await.unwrap();
    assert_eq!(removed.name, "topic");
    assert_eq!(removed.checkpoint.branch.as_deref(), Some("topic"));
    assert_eq!(removed.checkpoint.head, head.trim());
    assert_eq!(removed.checkpoint.cause, CheckpointCause::Removal);
    assert!(!removed.checkpoint.files_omitted);
    assert_eq!(removed.cause, EventCause::Manual);
    assert_eq!(
        removed.links.issue.as_deref(),
        Some("https://forge/issue/1")
    );
    assert_eq!(removed.expires_at, removed.checkpoint.expires_at);
    assert_eq!(
        removed.checkpoint.expires_at - removed.checkpoint.created_at,
        30 * 24 * 3600
    );
    let reference = git::checkpoint::reference(&removed.checkpoint.commit);
    assert_eq!(
        git(&checkout, &["rev-parse", &reference]).trim(),
        removed.checkpoint.commit
    );
    assert_eq!(
        git(
            &checkout,
            &["show", &format!("{}:scratch", removed.checkpoint.commit)]
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

    // Expiry deletes the record, its checkpoint and its ref, and refs
    // nothing owns.
    git(
        &checkout,
        &["update-ref", "refs/shoal/removed/unknown", "HEAD"],
    );
    git(
        &checkout,
        &["update-ref", "refs/shoal/checkpoints/unknown/1", "HEAD"],
    );
    manager.expire_checkpoints().await.unwrap();
    assert!(record(&manager, &workspace.id).await.is_some());
    assert_eq!(checkpoint_refs(&checkout), [reference]);
    let id = workspace.id.clone();
    manager
        .store
        .run(move |db| {
            db.execute(
                "UPDATE removed_workspaces SET expires_at=0 WHERE workspace_id=?1",
                [&id],
            )?;
            db.execute(
                "UPDATE checkpoints SET expires_at=0 WHERE workspace_id=?1",
                [&id],
            )?;
            Ok(())
        })
        .await
        .unwrap();
    manager.expire_checkpoints().await.unwrap();
    assert!(record(&manager, &workspace.id).await.is_none());
    assert!(checkpoint_refs(&checkout).is_empty());
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
                [&id],
            )?;
            db.execute(
                "INSERT INTO workspace_items(workspace_id,relation,kind,url,title) VALUES
                    (?1,'opened','issue','https://github.com/acme/repo/issues/2','Follow-up'),
                    (?1,'related','pr','https://github.com/acme/repo/pull/3',NULL)",
                [&id],
            )?;
            Ok(())
        })
        .await
        .unwrap();
    let status = git(&workspace.path, &["status", "--porcelain"]);
    manager
        .remove_workspace(
            &workspace.id,
            UnsavedWork::All,
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
    let recorded = |items: &[crate::model::LinkedItem]| {
        items
            .iter()
            .map(|item| (item.url.clone(), item.title.clone()))
            .collect::<Vec<_>>()
    };
    assert_eq!(
        recorded(&workspace.links.opened),
        [(
            "https://github.com/acme/repo/issues/2".to_owned(),
            Some("Follow-up".to_owned())
        )]
    );
    assert_eq!(
        recorded(&workspace.links.related),
        [("https://github.com/acme/repo/pull/3".to_owned(), None)]
    );
    assert!(manager.removed_workspaces().await.unwrap().is_empty());
    // The removal's checkpoint belongs to the new workspace.
    let checkpoints = manager.checkpoints("topic", None).await.unwrap().unwrap();
    assert_eq!(checkpoints.len(), 1);
    assert_eq!(checkpoints[0].workspace_id, workspace.id);
    assert_eq!(checkpoints[0].cause, CheckpointCause::Removal);
    // Its numbering continues after the moved checkpoints.
    let next = manager.take_checkpoint("topic", None).await.unwrap();
    assert_eq!(next.number, checkpoints[0].number + 1);
    assert!(
        checkpoint_refs(&checkout).contains(&git::checkpoint::reference(&checkpoints[0].commit))
    );
}

#[tokio::test]
async fn a_started_restore_keeps_expiry_from_its_record_and_checkpoints() {
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
    manager
        .remove_workspace(
            &workspace.id,
            UnsavedWork::All,
            InspectionPolicy::IncludeDirectoryProcesses,
        )
        .await
        .unwrap();
    // Both expire before a restore's claim would end.
    let id = workspace.id.clone();
    let soon = (crate::time::unix_seconds() + RESTORE_CLAIM.as_secs() / 2) as i64;
    manager
        .store
        .run(move |db| {
            db.execute(
                "UPDATE removed_workspaces SET expires_at=?2, record=json_set(record,
                    '$.expires_at', ?2, '$.checkpoint.expires_at', ?2) WHERE workspace_id=?1",
                rusqlite::params![id, soon],
            )?;
            db.execute(
                "UPDATE checkpoints SET expires_at=?2, record=json_set(record, '$.expires_at', ?2)
                 WHERE workspace_id=?1",
                rusqlite::params![id, soon],
            )?;
            Ok(())
        })
        .await
        .unwrap();
    let floor = crate::time::unix_seconds() + RESTORE_CLAIM.as_secs();
    let claimed = manager.claim_removed_workspace("topic").await.unwrap();
    assert!(claimed.expires_at >= floor);
    assert!(claimed.checkpoint.expires_at >= floor);
    let saved = record(&manager, &workspace.id).await.unwrap();
    assert_eq!(saved.expires_at, claimed.expires_at);
    assert_eq!(saved.checkpoint.expires_at, claimed.checkpoint.expires_at);
    let id = workspace.id.clone();
    let checkpoint_expiry: i64 = manager
        .store
        .run(move |db| {
            Ok(db.query_row(
                "SELECT expires_at FROM checkpoints WHERE workspace_id=?1",
                [id],
                |row| row.get(0),
            )?)
        })
        .await
        .unwrap();
    assert_eq!(checkpoint_expiry as u64, claimed.checkpoint.expires_at);
    manager.restore_workspace("topic").await.unwrap();
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
            UnsavedWork::All,
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
async fn a_removal_record_keeps_links_made_until_removal_starts() {
    use crate::{forge::link::ItemKind, model::Relation};
    let (root, manager) = manager().await;
    let checkout = repository(root.path(), "repo");
    let repo = manager
        .register_repository(checkout.to_str().unwrap().into(), None, None)
        .await
        .unwrap();
    // Removal works from this snapshot, taken before the links change.
    let workspace = manager
        .create_workspace(&repo.id, "topic".into(), None, None, None)
        .await
        .unwrap();
    git(
        &checkout,
        &["remote", "add", "origin", "https://github.com/acme/repo"],
    );
    manager
        .link_item(&workspace.id, ItemKind::Pr, "3", Relation::Related, None)
        .await
        .unwrap();
    manager
        .link_item(&workspace.id, ItemKind::Pr, "4", Relation::Related, None)
        .await
        .unwrap();
    manager
        .unlink_items(
            &workspace.id,
            Relation::Related,
            Some(ItemKind::Pr),
            Some("4"),
        )
        .await
        .unwrap();
    manager
        .link_item(
            &workspace.id,
            ItemKind::Issue,
            "7",
            Relation::Assigned,
            None,
        )
        .await
        .unwrap();
    manager
        .reserve_lifecycle(&workspace.id, crate::state::WorkspaceState::Removing)
        .await
        .unwrap();
    assert!(
        manager
            .unlink_items(
                &workspace.id,
                Relation::Assigned,
                Some(ItemKind::Issue),
                None
            )
            .await
            .is_err()
    );
    let error = manager
        .link_item(&workspace.id, ItemKind::Pr, "5", Relation::Related, None)
        .await
        .unwrap_err();
    assert!(
        format!("{error:#}").contains("cannot change links"),
        "{error:#}"
    );
    assert!(
        manager
            .unlink_items(&workspace.id, Relation::Related, None, None)
            .await
            .is_err()
    );
    manager
        .save_removal_record(&workspace, EventCause::Manual)
        .await
        .unwrap();
    let saved = record(&manager, &workspace.id).await.unwrap();
    let urls: Vec<_> = saved
        .links
        .related
        .iter()
        .map(|item| item.url.as_str())
        .collect();
    assert_eq!(urls, ["https://github.com/acme/repo/pull/3"]);
    assert_eq!(
        saved.links.issue.as_deref(),
        Some("https://github.com/acme/repo/issues/7")
    );
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
    manager
        .save_removal_record(&workspace, EventCause::Idle)
        .await
        .unwrap();
    assert!(
        manager.removed_workspaces().await.unwrap().is_empty(),
        "the workspace still exists"
    );
    // The files go, then the daemon stops before the removal commits; the
    // next sweeps forget the deleted worktree and keep its checkpoint.
    fs::remove_dir_all(&workspace.path).unwrap();
    crate::daemon::cleanup::sweep(&manager, &mut Default::default())
        .await
        .unwrap();
    assert!(manager.workspace(&workspace.id).await.is_err());
    manager.expire_checkpoints().await.unwrap();
    assert_eq!(manager.removed_workspaces().await.unwrap().len(), 1);

    let restored = manager.restore_workspace("topic").await.unwrap();
    let path = &restored.opened.workspace.path;
    assert_eq!(fs::read_to_string(path.join("scratch")).unwrap(), "notes\n");
    assert_eq!(fs::read_to_string(path.join("work")).unwrap(), "work\n");
}

#[tokio::test]
async fn a_failed_removal_keeps_its_checkpoint() {
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
    // An unreadable Worktrunk config fails the removal after the checkpoint
    // is saved.
    let config = manager.paths.worktrunk_config();
    let valid = fs::read_to_string(&config).unwrap();
    fs::write(&config, "not [valid toml").unwrap();
    let removed = manager
        .remove_workspace(
            &workspace.id,
            UnsavedWork::All,
            InspectionPolicy::IncludeDirectoryProcesses,
        )
        .await;
    fs::write(&config, valid).unwrap();
    assert!(removed.is_err());
    let record = record(&manager, &workspace.id).await.unwrap();
    assert_eq!(
        git(
            &checkout,
            &["show", &format!("{}:scratch", record.checkpoint.commit)]
        ),
        "notes\n"
    );
    assert_eq!(
        checkpoint_refs(&checkout),
        [git::checkpoint::reference(&record.checkpoint.commit)]
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
            UnsavedWork::All,
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
async fn a_retried_removal_keeps_earlier_checkpoints_and_reuses_an_unchanged_one() {
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
        manager
            .save_removal_record(&workspace, EventCause::Manual)
            .await
            .unwrap()
            .checkpoint
    };
    fs::write(workspace.path.join("first"), "first\n").unwrap();
    let first = save().await;
    // The failed attempt deleted the file; the retry finds the rest.
    fs::remove_file(workspace.path.join("first")).unwrap();
    fs::write(workspace.path.join("second"), "second\n").unwrap();
    let second = save().await;
    assert_eq!((first.number, second.number), (1, 2));
    assert_eq!(
        git(&checkout, &["show", &format!("{}:first", first.commit)]),
        "first\n"
    );
    assert_eq!(
        record(&manager, &workspace.id).await.unwrap().checkpoint,
        second
    );
    // An unchanged retry extends the latest checkpoint instead.
    let third = save().await;
    assert_eq!(third.number, second.number);
    let mut kept = vec![
        git::checkpoint::reference(&first.commit),
        git::checkpoint::reference(&second.commit),
    ];
    kept.sort();
    assert_eq!(checkpoint_refs(&checkout), kept);
}
