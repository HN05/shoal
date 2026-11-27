use crate::git;
use crate::model::EventCause;
use crate::model::RemovedSpace;
use crate::refusal::Reason;
use crate::restore::*;
use crate::scope::Caller;
use crate::stand_in::Fixture;
use crate::transfer::MoveLock;
use shoal_core::test_support::{commit, git, repository};
use shoal_daemon_base::callers::Access;
use shoal_ids::space_ids::SpaceId;
use shoal_refs::BranchName;
use std::fs;

use crate::removal::{InspectionPolicy, UnsavedWork};
use shoal_protocol::space_names::SpaceSelector;

/// Sorted.
fn checkpoint_refs(repo: &std::path::Path) -> Vec<String> {
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

async fn record(manager: &Fixture, id: &str) -> Option<RemovedSpace> {
    let id = id.to_owned();
    manager
        .store
        .run(move |db| super::records::stored(db, &id.into()))
        .await
        .unwrap()
}

#[tokio::test]
async fn removal_checkpoint_keeps_files_workspace_processes_write_as_they_stop() {
    let manager = Fixture::new().await;
    let root = &manager.root;
    let checkout = repository(root.path(), "repo");
    let repo = manager
        .register_repository(checkout.to_str().unwrap().into(), None, None)
        .await
        .unwrap();
    let workspace = manager
        .create_workspace(&repo.id, "topic".into(), None, None, None)
        .await
        .unwrap();
    // A process started with the workspace identity, as hooks and `shoal
    // internal env` exports start them, that saves its work when asked to
    // stop.
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
        .env(crate::env::SPACE, &workspace.id)
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
        .spaces
        .remove_selected(
            &*manager,
            &SpaceSelector::id(&workspace.id),
            UnsavedWork::All,
            InspectionPolicy::GitOnly,
        )
        .await
        .unwrap();

    assert!(process.wait().await.unwrap().success());
    let removed = record(&manager, workspace.id.as_str()).await.unwrap();
    assert_eq!(
        git(
            &checkout,
            &[
                "show",
                &format!(
                    "{}:flushed",
                    removed.kept_workspace().unwrap().checkpoint.commit
                )
            ]
        ),
        "flushed\n"
    );
}

#[tokio::test]
async fn a_started_restore_keeps_expiry_from_its_record_checkpoints_and_files() {
    let manager = Fixture::new().await;
    let root = &manager.root;
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
        .spaces
        .remove_selected(
            &*manager,
            &SpaceSelector::id(&workspace.id),
            UnsavedWork::All,
            InspectionPolicy::IncludeDirectoryProcesses,
        )
        .await
        .unwrap();
    // All expire before a restore's claim would end.
    let id = workspace.id.clone();
    let soon = crate::time::UnixTime::now() + RESTORE_CLAIM / 2;
    manager
        .store
        .run(move |db| {
            db.execute(
                "UPDATE removed_spaces SET expires_at=?2, checkpoint_expires_at=?2
                    WHERE space_id=?1",
                rusqlite::params![id, soon],
            )?;
            db.execute(
                "UPDATE checkpoints SET expires_at=?2 WHERE workspace_id=?1",
                rusqlite::params![id, soon],
            )?;
            db.execute(
                "INSERT INTO removed_space_files(space_id,chat,expires_at) VALUES (?1,0,?2)",
                rusqlite::params![id, soon],
            )?;
            Ok(())
        })
        .await
        .unwrap();
    let floor = crate::time::UnixTime::now() + RESTORE_CLAIM;
    let claimed = manager
        .spaces
        .claim_removed_space(&manager.named("topic").await)
        .await
        .unwrap();
    assert!(claimed.expires_at >= floor);
    assert!(claimed.kept_workspace().unwrap().checkpoint.expires_at >= floor);
    let saved = record(&manager, workspace.id.as_str()).await.unwrap();
    assert_eq!(saved.expires_at, claimed.expires_at);
    assert_eq!(
        saved.kept_workspace().unwrap().checkpoint.expires_at,
        claimed.kept_workspace().unwrap().checkpoint.expires_at
    );
    let id = workspace.id.clone();
    let (checkpoint_expiry, files_expiry): (crate::time::UnixTime, crate::time::UnixTime) = manager
        .store
        .run(move |db| {
            Ok(db.query_row(
                "SELECT expires_at,
                        (SELECT expires_at FROM removed_space_files WHERE space_id=?1)
                    FROM checkpoints WHERE workspace_id=?1",
                [id],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )?)
        })
        .await
        .unwrap();
    assert_eq!(
        checkpoint_expiry,
        claimed.kept_workspace().unwrap().checkpoint.expires_at
    );
    assert!(files_expiry >= floor);
    manager
        .restore_workspace(&manager.named("topic").await)
        .await
        .unwrap();
}

#[tokio::test]
async fn restore_refuses_a_branch_in_use() {
    let manager = Fixture::new().await;
    let root = &manager.root;
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
        .spaces
        .remove_selected(
            &*manager,
            &SpaceSelector::id(&workspace.id),
            UnsavedWork::All,
            InspectionPolicy::IncludeDirectoryProcesses,
        )
        .await
        .unwrap();
    git(&checkout, &["branch", "-D", "topic"]);
    manager
        .create_workspace(&repo.id, "topic".into(), None, None, None)
        .await
        .unwrap();
    assert!(
        manager
            .restore_workspace(&manager.named("topic").await)
            .await
            .is_err()
    );
    assert_eq!(manager.spaces.removed_workspaces().await.unwrap().len(), 1);
}

#[tokio::test]
async fn restore_reopens_the_branch_of_a_workspace_removed_with_a_detached_head() {
    let manager = Fixture::new().await;
    let root = &manager.root;
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
    // As during a rebase, HEAD is off the branch when the removal comes.
    git(&workspace.path, &["checkout", "--detach"]);
    manager
        .spaces
        .remove_selected(
            &*manager,
            &SpaceSelector::id(&workspace.id),
            UnsavedWork::All,
            InspectionPolicy::IncludeDirectoryProcesses,
        )
        .await
        .unwrap();

    let restored = manager
        .restore_workspace(&manager.named("topic").await)
        .await
        .unwrap();
    assert_eq!(
        restored.opened.workspace.branch().map(BranchName::as_str),
        Some("topic")
    );
}

#[tokio::test]
async fn restore_reopens_a_head_its_branch_lost_on_a_branch_of_its_own() {
    let manager = Fixture::new().await;
    let root = &manager.root;
    let checkout = repository(root.path(), "repo");
    let repo = manager
        .register_repository(checkout.to_str().unwrap().into(), None, None)
        .await
        .unwrap();
    let workspace = manager
        .create_workspace(&repo.id, "topic".into(), None, None, None)
        .await
        .unwrap();
    let tip = git(&checkout, &["rev-parse", "topic"]);
    // As in a rebase, HEAD left the branch and gained a commit it lacks.
    git(&workspace.path, &["checkout", "--detach"]);
    fs::write(workspace.path.join("work"), "work\n").unwrap();
    commit(&workspace.path, "work");
    let head = git(&workspace.path, &["rev-parse", "HEAD"]);
    manager
        .spaces
        .remove_selected(
            &*manager,
            &SpaceSelector::id(&workspace.id),
            UnsavedWork::All,
            InspectionPolicy::IncludeDirectoryProcesses,
        )
        .await
        .unwrap();

    let restored = manager
        .restore_workspace(&manager.named("topic").await)
        .await
        .unwrap();
    let reopened = &restored.opened.workspace;
    assert_eq!(reopened.branch().map(BranchName::as_str), Some(&head[..7]));
    assert_eq!(git(&reopened.path, &["rev-parse", "HEAD"]), head);
    assert_eq!(git(&checkout, &["rev-parse", "topic"]), tip);
    assert_eq!(restored.warnings.len(), 1, "{:?}", restored.warnings);
}

#[tokio::test]
async fn a_failed_removal_keeps_its_checkpoint() {
    let manager = Fixture::new().await;
    let root = &manager.root;
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
    // Git refuses to remove a locked worktree, which fails the removal after
    // the checkpoint is saved.
    let path = workspace.path.to_str().unwrap();
    git(&checkout, &["worktree", "lock", path]);
    let removed = manager
        .spaces
        .remove_selected(
            &*manager,
            &SpaceSelector::id(&workspace.id),
            UnsavedWork::All,
            InspectionPolicy::IncludeDirectoryProcesses,
        )
        .await;
    git(&checkout, &["worktree", "unlock", path]);
    assert!(removed.is_err());
    let record = record(&manager, workspace.id.as_str()).await.unwrap();
    assert_eq!(
        git(
            &checkout,
            &[
                "show",
                &format!(
                    "{}:scratch",
                    record.kept_workspace().unwrap().checkpoint.commit
                )
            ]
        ),
        "notes\n"
    );
    assert_eq!(
        checkpoint_refs(&checkout),
        [git::checkpoint::reference(
            &record.kept_workspace().unwrap().checkpoint.commit
        )]
    );
    assert!(
        manager
            .spaces
            .removed_workspaces()
            .await
            .unwrap()
            .is_empty(),
        "the workspace still exists"
    );
}

/// A workspace created without setup comes back without it.
#[tokio::test]
async fn restore_skips_setup_a_workspace_skipped() {
    let manager = Fixture::new().await;
    let root = &manager.root;
    let checkout = repository(root.path(), "repo");
    let repo = manager
        .register_repository(checkout.to_str().unwrap().into(), None, None)
        .await
        .unwrap();
    let new = crate::workspace::NewWorkspace {
        options: crate::protocol::CreationOptions {
            skip_setup: true,
            ..Default::default()
        },
        ..Default::default()
    };
    let workspace = manager
        .spaces
        .create_workspace_with(&*manager, &repo.id, "topic".into(), None, new)
        .await
        .unwrap();
    manager
        .spaces
        .remove_selected(
            &*manager,
            &SpaceSelector::id(&workspace.id),
            UnsavedWork::All,
            InspectionPolicy::IncludeDirectoryProcesses,
        )
        .await
        .unwrap();

    let restored = manager
        .restore_workspace(&manager.named("topic").await)
        .await
        .unwrap();
    assert!(restored.opened.workspace.setup_skipped);
}

#[tokio::test]
async fn restore_keeps_the_recorded_base_and_its_stack() {
    let manager = Fixture::new().await;
    let root = &manager.root;
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
    assert_eq!(upper.checkout.base_ref(), Some("refs/heads/lower"));
    manager
        .spaces
        .remove_selected(
            &*manager,
            &SpaceSelector::id(&upper.id),
            UnsavedWork::All,
            InspectionPolicy::IncludeDirectoryProcesses,
        )
        .await
        .unwrap();
    manager
        .restore_workspace(&manager.named("upper").await)
        .await
        .unwrap();
    let restored = manager
        .spaces
        .workspace(&manager.named("upper").await)
        .await
        .unwrap();
    assert_eq!(restored.checkout.base_ref(), Some("refs/heads/lower"));
    assert_eq!(
        restored
            .part
            .base_workspace
            .map(|base| base.name)
            .as_deref(),
        Some("lower")
    );
}

#[tokio::test]
async fn a_retried_removal_keeps_earlier_checkpoints_and_reuses_an_unchanged_one() {
    let manager = Fixture::new().await;
    let root = &manager.root;
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
            .spaces
            .save_removal_record(&*manager, &workspace, EventCause::Manual, None)
            .await
            .unwrap()
            .kept_workspace()
            .unwrap()
            .checkpoint
            .clone()
    };
    fs::write(workspace.path.join("first"), "first\n").unwrap();
    let first = save().await;
    // The failed attempt deleted the file; the retry finds the rest.
    fs::remove_file(workspace.path.join("first")).unwrap();
    fs::write(workspace.path.join("second"), "second\n").unwrap();
    let second = save().await;
    assert_eq!((first.number, second.number), (1.into(), 2.into()));
    assert_eq!(
        git(&checkout, &["show", &format!("{}:first", first.commit)]),
        "first\n"
    );
    assert_eq!(
        record(&manager, workspace.id.as_str())
            .await
            .unwrap()
            .kept_workspace()
            .unwrap()
            .checkpoint,
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

#[tokio::test]
async fn restore_gives_the_workspace_the_files_the_removed_one_kept() {
    use crate::workspace_files::Attacher;
    let manager = Fixture::new().await;
    let root = &manager.root;
    let checkout = repository(root.path(), "repo");
    let repo = manager
        .register_repository(checkout.to_str().unwrap().into(), None, None)
        .await
        .unwrap();
    let parent = manager
        .create_workspace(&repo.id, "parent".into(), None, None, None)
        .await
        .unwrap();
    let workspace = manager
        .spaces
        .create_child(
            &*manager,
            &SpaceSelector::id(&parent.id),
            "topic".into(),
            None,
            Default::default(),
        )
        .await
        .unwrap();
    let file = root.path().join("notes.md");
    fs::write(&file, "notes").unwrap();
    let added = manager
        .spaces
        .add_attached_files(
            &*manager,
            &SpaceSelector::id(&workspace.id),
            vec![file.into()],
            &Attacher::User,
        )
        .await
        .unwrap();
    let [added] = added.as_slice() else {
        panic!("{added:?}");
    };
    manager
        .spaces
        .remove_selected(
            &*manager,
            &SpaceSelector::id(&workspace.id),
            UnsavedWork::All,
            InspectionPolicy::GitOnly,
        )
        .await
        .unwrap();
    // Disk cleanup lists the removed workspace before the restore.
    let listed = manager.spaces.kept_results().await.unwrap();
    let restored = manager
        .restore_workspace(&SpaceSelector::id(&workspace.id))
        .await
        .unwrap();
    let id = SpaceSelector::id(&restored.opened.workspace.id);

    // They stay where the agent's session saw them, as the workspace's own.
    manager.spaces.expire_results(&*manager).await.unwrap();
    let files = manager.spaces.attached_files(&id).await.unwrap();
    let [file] = files.as_slice() else {
        panic!("{files:?}");
    };
    assert_eq!(
        (&file.path().unwrap(), file.expires_at),
        (&added.path().unwrap(), None)
    );
    // The restored workspace goes below its parent again, which reaches
    // them as before.
    let parents: Vec<Option<String>> = manager
        .store
        .run(|db| {
            Ok(db
                .prepare("SELECT parent_id FROM space_files")?
                .query_map([], |row| row.get(0))?
                .collect::<rusqlite::Result<_>>()?)
        })
        .await
        .unwrap();
    assert_eq!(parents, [Some(parent.id.to_string())]);
    let [listed] = listed.as_slice() else {
        panic!("{listed:?}");
    };
    assert!(
        !manager
            .spaces
            .delete_results_in(&*manager, listed, |_| true)
            .await
            .unwrap()
    );
    assert!(added.path().unwrap().exists());

    // Removed again, they expire with it.
    manager
        .spaces
        .remove_selected(&*manager, &id, UnsavedWork::All, InspectionPolicy::GitOnly)
        .await
        .unwrap();
    manager
        .store
        .run(|db| {
            db.execute("UPDATE removed_space_files SET expires_at=0", [])?;
            Ok(())
        })
        .await
        .unwrap();
    manager.spaces.expire_results(&*manager).await.unwrap();
    assert!(!added.path().unwrap().exists());
    let removed_id = added.path().unwrap().ancestors().nth(3).unwrap();
    assert!(removed_id.ends_with(workspace.id.as_str()));
    assert!(!removed_id.exists());
}

#[tokio::test]
async fn a_scoped_caller_restores_only_below_a_parent_in_its_scope() {
    let manager = Fixture::new().await;
    let root = &manager.root;
    let checkout = repository(root.path(), "repo");
    let repo = manager
        .register_repository(checkout.to_str().unwrap().into(), None, None)
        .await
        .unwrap();
    let parent = manager
        .create_workspace(&repo.id, "parent".into(), None, None, None)
        .await
        .unwrap();
    let child = manager
        .spaces
        .create_child(
            &*manager,
            &SpaceSelector::id(&parent.id),
            "child".into(),
            None,
            Default::default(),
        )
        .await
        .unwrap();
    let top = manager
        .create_workspace(&repo.id, "top".into(), None, None, None)
        .await
        .unwrap();
    for id in [&child.id, &top.id] {
        let selector = SpaceSelector::id(id);
        (manager.spaces.remove_selected(
            &*manager,
            &selector,
            UnsavedWork::All,
            InspectionPolicy::GitOnly,
        ))
        .await
        .unwrap();
    }
    let caller = Caller {
        space_id: parent.id.clone(),
        execution: None,
        granted: None,
        access: Access::Full,
        vouched: None,
    };

    // It sees and restores only the child, whose parent its scope reaches.
    let listed = manager
        .spaces
        .restorable_spaces(&*manager, Some(&caller))
        .await
        .unwrap();
    let listed: Vec<_> = listed.iter().map(|removed| &removed.space_id).collect();
    assert_eq!(listed, [&child.id]);
    let refused =
        (manager
            .spaces
            .ensure_restorable_by(&*manager, &SpaceSelector::id(&top.id), &caller))
        .await
        .unwrap_err();
    assert!(format!("{refused:#}").contains("the user's"), "{refused:#}");
    let selector = SpaceSelector::id(&child.id);
    manager
        .spaces
        .ensure_restorable_by(&*manager, &selector, &caller)
        .await
        .unwrap();
    manager
        .spaces
        .restore_space(&*manager, &selector, Some(&caller))
        .await
        .unwrap();
    let id = child.id.clone();
    let below = manager
        .store
        .read(move |tx| crate::tree::parent_id(tx, &id))
        .await
        .unwrap();
    assert_eq!(below, Some(parent.id));
}

#[tokio::test]
async fn a_restored_workspace_goes_below_its_parent_while_that_exists() {
    let manager = Fixture::new().await;
    let root = &manager.root;
    let checkout = repository(root.path(), "repo");
    let repo = manager
        .register_repository(checkout.to_str().unwrap().into(), None, None)
        .await
        .unwrap();
    let parent = manager
        .create_workspace(&repo.id, "parent".into(), None, None, None)
        .await
        .unwrap();
    let child = manager
        .spaces
        .create_child(
            &*manager,
            &SpaceSelector::id(&parent.id),
            "child".into(),
            None,
            Default::default(),
        )
        .await
        .unwrap();
    let child_selector = SpaceSelector::id(&child.id);
    let remove = |selector: SpaceSelector| {
        let manager = manager.daemon.clone();
        async move {
            (manager.spaces.remove_selected(
                &*manager,
                &selector,
                UnsavedWork::All,
                InspectionPolicy::GitOnly,
            ))
            .await
            .unwrap();
        }
    };
    let parent_of = |id: SpaceId| {
        let manager = manager.daemon.clone();
        async move {
            (manager.store)
                .read(move |tx| crate::tree::parent_id(tx, &id))
                .await
                .unwrap()
        }
    };
    remove(child_selector.clone()).await;
    let restored = manager.restore_workspace(&child_selector).await.unwrap();
    assert!(restored.warnings.is_empty(), "{:?}", restored.warnings);
    assert_eq!(parent_of(child.id.clone()).await, Some(parent.id.clone()));

    // Removed with its parent, it restores alone, at the top level.
    remove(SpaceSelector::id(&parent.id)).await;
    let restored = manager.restore_workspace(&child_selector).await.unwrap();
    assert_eq!(restored.warnings.len(), 1, "{:?}", restored.warnings);
    assert!(restored.warnings[0].contains(parent.id.as_str()));
    assert_eq!(parent_of(child.id.clone()).await, None);
    let removed = manager.spaces.removed_workspaces().await.unwrap();
    assert_eq!(removed.len(), 1);
    assert_eq!(removed[0].space_id, parent.id);
}

#[tokio::test]
async fn a_restore_whose_worktree_fails_finishes_and_retries_it_with_its_unsaved_files() {
    let manager = Fixture::new().await;
    let root = &manager.root;
    let checkout = repository(root.path(), "repo");
    let repo = manager
        .register_repository(checkout.to_str().unwrap().into(), None, None)
        .await
        .unwrap();
    let workspace = manager
        .create_workspace(&repo.id, "topic".into(), None, None, None)
        .await
        .unwrap();
    fs::write(workspace.path.join("unsaved"), "kept").unwrap();
    let selector = SpaceSelector::id(&workspace.id);
    manager
        .spaces
        .remove_selected(
            &*manager,
            &selector,
            UnsavedWork::All,
            InspectionPolicy::GitOnly,
        )
        .await
        .unwrap();
    let blocked = crate::stand_in::block_worktrees(&checkout);
    // The restore goes on without its worktree, which a warning names, and
    // takes its removal record.
    let restored = manager.restore_workspace(&selector).await.unwrap();
    drop(blocked);
    assert_eq!(restored.warnings.len(), 1, "{:?}", restored.warnings);
    assert!(record(&manager, workspace.id.as_str()).await.is_none());
    let restored = manager.spaces.workspace(&selector).await.unwrap();
    assert_eq!(restored.state, crate::state::WorkspaceState::Failed);
    assert!(restored.checkout.is_pending());
    let created = manager
        .spaces
        .create_pending_worktree(&*manager, &selector)
        .await
        .unwrap();
    let kept = fs::read_to_string(created.path.join("unsaved")).unwrap();
    assert_eq!(kept, "kept");
    assert_eq!(
        git(&created.path, &["branch", "--show-current"]).trim(),
        "topic"
    );
    manager.store.shutdown().await;
}

#[tokio::test]
async fn restore_refuses_a_record_while_its_move_to_another_machine_runs() {
    let manager = Fixture::new().await;
    let root = &manager.root;
    let checkout = repository(root.path(), "repo");
    let repo = manager
        .register_repository(checkout.to_str().unwrap().into(), None, None)
        .await
        .unwrap();
    let workspace = manager
        .create_workspace(&repo.id, "topic".into(), None, None, None)
        .await
        .unwrap();
    let selector = SpaceSelector::id(&workspace.id);
    let removal = UnsavedWork::All;
    let inspection = InspectionPolicy::IncludeDirectoryProcesses;
    (manager
        .spaces
        .remove_selected(&*manager, &selector, removal, inspection))
    .await
    .unwrap();
    let moving = MoveLock::try_take(&manager.paths, &workspace.id)
        .unwrap()
        .unwrap();

    let refused = manager
        .spaces
        .restore_space(&*manager, &selector, None)
        .await
        .unwrap_err();

    assert_eq!(Reason::of(&refused), Some(Reason::MoveInProgress));
    assert!(record(&manager, workspace.id.as_str()).await.is_some());
    moving.release();
    manager
        .spaces
        .restore_space(&*manager, &selector, None)
        .await
        .unwrap();
}

#[tokio::test]
async fn an_agent_takes_a_space_over_only_as_its_admin_permission_allows() {
    use crate::approvals::{Target, Undecided};
    use crate::config::permissions::PermissionKey;

    let caller = |space: &SpaceId| Caller {
        space_id: space.clone(),
        execution: None,
        granted: None,
        access: Access::Full,
        vouched: None,
    };
    let manager = Fixture::new().await;
    let master = manager.chat("master", "orchestrate", None, None).await;
    let worker = manager.chat("worker", "chat", None, Some(&master.id)).await;
    let elsewhere = SpaceSelector::id(&SpaceId::from("elsewhere".to_owned()));
    // The user and a master session take it over at once; a chat below
    // asks its coordinator.
    (manager
        .spaces
        .ensure_takeover_allowed(&*manager, None, &elsewhere)
        .await)
        .unwrap();
    (manager
        .spaces
        .ensure_takeover_allowed(&*manager, Some(&caller(&master.id)), &elsewhere))
    .await
    .unwrap();
    let worker = caller(&worker.id);
    let asked = manager
        .spaces
        .ensure_takeover_allowed(&*manager, Some(&worker), &elsewhere);
    let Undecided(request) = asked.await.unwrap_err().downcast::<Undecided>().unwrap();
    assert_eq!(request.target, Target::Action(PermissionKey::Admin));
}

async fn restore_chat(manager: &Fixture, id: &SpaceId) -> crate::model::RestoredChat {
    match (manager.spaces)
        .restore_space(&**manager, &SpaceSelector::id(id), None)
        .await
    {
        Ok(crate::model::RestoredSpace::Chat(restored)) => *restored,
        other => panic!("{other:?}"),
    }
}

#[tokio::test]
async fn a_chat_in_a_removed_workspace_restores_once_that_workspace_does() {
    let manager = Fixture::new().await;
    let repo = manager.repository("repo").await;
    let workspace = manager
        .create_workspace(&repo.id, "parent".into(), None, None, None)
        .await
        .unwrap();
    let selector = SpaceSelector::id(&workspace.id);
    let removed = manager.chat_in(&selector, "review").await.unwrap();
    (manager.spaces.remove_selected(
        &*manager,
        &selector,
        UnsavedWork::All,
        InspectionPolicy::GitOnly,
    ))
    .await
    .unwrap();
    assert_eq!(manager.spaces.removed_spaces().await.unwrap().len(), 2);

    let refused = (manager
        .spaces
        .restore_space(&*manager, &SpaceSelector::id(&removed.id), None))
    .await
    .unwrap_err();
    let command = format!("shoal restore {} {}", workspace.id, removed.id);
    assert!(format!("{refused:#}").contains(&command), "{refused:#}");

    manager.restore_workspace(&selector).await.unwrap();
    let restored = restore_chat(&manager, &removed.id).await;
    let place = restored
        .chat
        .workdir
        .workspace()
        .map(|workspace| workspace.id.clone());
    assert_eq!(place, Some(workspace.id));
}

#[tokio::test]
async fn a_chat_without_a_repository_gets_back_the_session_files_its_root_dir_kept() {
    let manager = Fixture::new().await;
    let root = &manager.root;
    let removed = manager.chat("notes", "chat", None, None).await;
    manager
        .spaces
        .remove_selected(
            &*manager,
            &SpaceSelector::id(&removed.id),
            UnsavedWork::default(),
            InspectionPolicy::GitOnly,
        )
        .await
        .unwrap();
    let record = manager.spaces.removed_spaces().await.unwrap().remove(0);
    let contents = root.path().join("carried");
    std::fs::write(&contents, "{}\n").unwrap();
    let carried = shoal_protocol::transcripts::CarriedSessionFile {
        file: shoal_protocol::transcripts::SessionFile {
            agent: "claude".into(),
            session: "s1".into(),
            path: "project/s1.jsonl".into(),
            size: 3,
        },
        contents,
    };
    let record = manager
        .spaces
        .keep_imported_sessions(&*manager, record, std::slice::from_ref(&carried))
        .await
        .unwrap();
    let kept = record.clone();
    (manager.store)
        .run(move |db| crate::restore::records::record(db, &kept))
        .await
        .unwrap();

    let restored = restore_chat(&manager, &removed.id).await;
    let [handed] = restored.session_files.as_slice() else {
        panic!("{:?}", restored.session_files);
    };
    assert_eq!(handed.file, carried.file);
    let root_dir = record.chat().unwrap().root_dir.clone().unwrap();
    assert!(handed.contents.starts_with(root_dir.join(".sessions")));
    assert_eq!(std::fs::read_to_string(&handed.contents).unwrap(), "{}\n");

    // Once the caller put them in place, the daemon deletes them.
    manager
        .spaces
        .drop_restored_sessions(&*manager, &SpaceSelector::id(&removed.id))
        .await
        .unwrap();
    assert!(!handed.contents.exists());
}
