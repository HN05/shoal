use super::*;
use crate::client::space_names::SpaceSelector;
use crate::daemon::workspace_files::Attacher;

#[test]
fn changed_signals_restart_timer_and_expiry_returns_its_snapshot() {
    let mut timers = Timers::default();
    let now = Instant::now();
    let delay = Duration::from_secs(600);
    let snapshot = Snapshot {
        rule: Rule::Idle,
        signals: 1,
        tree: 7,
    };
    assert!(matches!(
        timers.expired("w", 1, now, delay),
        Expiry::Restart
    ));
    timers.start("w", snapshot, now);
    assert!(matches!(
        timers.expired("w", 1, now + delay / 2, delay),
        Expiry::Running
    ));
    assert!(matches!(
        timers.expired("w", 2, now + delay, delay),
        Expiry::Restart
    ));
    assert!(matches!(
        timers.expired("w", 1, now + delay, delay),
        Expiry::Expired(expired) if expired == snapshot
    ));
}

#[test]
fn fingerprint_sees_rewrites_that_restore_the_modification_time() {
    let root = tempfile::tempdir().unwrap();
    let file = root.path().join("build-output");
    fs::write(&file, "before").unwrap();
    let metadata = fs::metadata(&file).unwrap();
    let modified = metadata.modified().unwrap();
    let change_time = |metadata: fs::Metadata| (metadata.ctime(), metadata.ctime_nsec());
    let changed = change_time(metadata);
    let before = walk(root.path()).unwrap().fingerprint;
    // Filesystem timestamps are coarse; rewrite until the change time moves.
    while change_time(fs::metadata(&file).unwrap()) == changed {
        fs::write(&file, "after!").unwrap();
        fs::File::options()
            .write(true)
            .open(&file)
            .unwrap()
            .set_modified(modified)
            .unwrap();
    }
    assert_eq!(fs::metadata(&file).unwrap().modified().unwrap(), modified);
    assert_ne!(walk(root.path()).unwrap().fingerprint, before);
}

#[tokio::test]
async fn deleted_parent_sweep_releases_leases_and_keeps_a_branch_with_work() {
    use crate::daemon::{
        ports::PortRequest,
        resources::ResourceRequest,
        test_support::{git, manager, repository},
    };
    for pending_rename in [false, true] {
        let (temp, manager) = manager().await;
        let checkout = repository(temp.path(), "repo");
        fs::write(checkout.join(".shoal.toml"), "[resources.lock]\n").unwrap();
        crate::daemon::test_support::commit(&checkout, ".shoal.toml");
        let repo = manager
            .register_repository(checkout.to_str().unwrap().into(), None, None)
            .await
            .unwrap();
        let workspace = manager
            .create_workspace(&repo.id, "deleted-parent".into(), None, None, None)
            .await
            .unwrap();
        manager
            .acquire_port(
                &SpaceSelector::id(&workspace.id),
                "web".into(),
                PortRequest::default(),
                None,
            )
            .await
            .unwrap();
        manager
            .acquire_resource(
                &SpaceSelector::id(&workspace.id),
                ResourceRequest {
                    mode: None,
                    kind: None,
                    pool: "lock".into(),
                    name: "lock".into(),
                    resource: None,
                    reason: None,
                    queue: false,
                },
                None,
            )
            .await
            .unwrap();
        // A commit only the branch has keeps it after the sweep.
        fs::write(workspace.path.join("work"), "work\n").unwrap();
        crate::daemon::test_support::commit(&workspace.path, "work");
        if pending_rename {
            git(&workspace.path, &["branch", "-m", "new/topic"]);
            let id = workspace.id.clone();
            manager.store.run(move |db| {
                db.execute("INSERT INTO workspace_renames(workspace_id,name,branch) VALUES (?1,'new-topic','new/topic')", [&id])?;
                db.execute("UPDATE workspaces SET state='failed' WHERE id=?1", [&id])?;
                Ok(())
            }).await.unwrap();
        }
        fs::remove_dir_all(workspace.path.parent().unwrap()).unwrap();

        sweep(&manager, &mut Timers::default()).await.unwrap();

        assert!(manager.workspace_by_id(&workspace.id).await.is_err());
        assert!(manager.list_ports(None).await.unwrap().is_empty());
        assert!(manager.list_resources(None).await.unwrap().is_empty());
        let branch = if pending_rename {
            "refs/heads/new/topic"
        } else {
            "refs/heads/deleted-parent"
        };
        assert!(!git(&checkout, &["rev-parse", branch]).is_empty());
        let remaining = manager
            .store
            .run(|db| {
                Ok(
                    db.query_row("SELECT COUNT(*) FROM workspace_renames", [], |row| {
                        row.get::<_, i64>(0)
                    })?,
                )
            })
            .await
            .unwrap();
        assert_eq!(remaining, 0);
        assert!(
            !git(&checkout, &["worktree", "list", "--porcelain"])
                .contains(workspace.path.to_str().unwrap())
        );
    }
}

#[tokio::test]
async fn failed_step_does_not_skip_later_steps() {
    use crate::daemon::test_support::{manager, repository};
    let (temp, manager) = manager().await;
    let checkout = repository(temp.path(), "repo");
    let repo = manager
        .register_repository(checkout.to_str().unwrap().into(), None, None)
        .await
        .unwrap();
    let done = manager
        .create_workspace(&repo.id, "done".into(), None, None, None)
        .await
        .unwrap();
    let idle = manager
        .create_workspace(&repo.id, "idle".into(), None, None, None)
        .await
        .unwrap();
    // Completion cleanup retains the dirty workspace and cannot record why.
    fs::write(done.path.join("work"), "retain").unwrap();
    manager
        .mark_done(
            &SpaceSelector::id(&done.id),
            true,
            false,
            vec![],
            &Attacher::User,
        )
        .await
        .unwrap();
    manager
        .store
        .run(|db| {
            db.execute_batch(
                "CREATE TEMP TRIGGER full_disk BEFORE UPDATE ON workspace_completion
                     BEGIN SELECT RAISE(ABORT, 'database or disk is full'); END;",
            )?;
            Ok(())
        })
        .await
        .unwrap();
    let mut timers = Timers::default();
    timers.idle.insert(
        idle.id.clone(),
        Idle {
            snapshot: manager.cleanup_snapshot(&idle.id).await.unwrap().unwrap(),
            since: Instant::now() - Duration::from_secs(3600),
        },
    );
    let failed = sweep(&manager, &mut timers).await.unwrap_err();
    assert_eq!(failed.general(), None);
    assert!(failed.space(&done.id).is_some());
    assert!(!idle.path.exists());
    assert!(done.path.exists());
}

#[tokio::test]
async fn invalid_pr_record_does_not_block_other_workspace_cleanup() {
    use crate::daemon::{
        links::pr::Action,
        test_support::{manager, repository},
    };
    let (temp, manager) = manager().await;
    let repository_dir = repository(temp.path(), "repo");
    let repo = manager
        .register_repository(repository_dir.to_str().unwrap().into(), None, None)
        .await
        .unwrap();
    // Names put the invalid record first in the sweep.
    let invalid = manager
        .create_workspace(&repo.id, "a-invalid".into(), None, None, None)
        .await
        .unwrap();
    let merged = manager
        .create_workspace(&repo.id, "b-merged".into(), None, None, None)
        .await
        .unwrap();
    let idle = manager
        .create_workspace(&repo.id, "c-idle".into(), None, None, None)
        .await
        .unwrap();
    manager
        .set_pr(&SpaceSelector::id(&merged.id), Action::Acknowledge)
        .await
        .unwrap();
    let id = invalid.id.clone();
    let record = r#"{"url":null,"head":null,"error":null}"#;
    manager
        .store
        .run(move |db| {
            db.execute(
                "INSERT INTO pr_cleanup(space_id,record) VALUES (?1,?2)",
                rusqlite::params![id, record],
            )?;
            Ok(())
        })
        .await
        .unwrap();
    let mut timers = Timers::default();
    timers.idle.insert(
        idle.id.clone(),
        Idle {
            snapshot: manager.cleanup_snapshot(&idle.id).await.unwrap().unwrap(),
            since: Instant::now() - Duration::from_secs(3600),
        },
    );
    sweep(&manager, &mut timers).await.unwrap();
    assert!(invalid.path.exists());
    assert!(!merged.path.exists());
    assert!(!idle.path.exists());
    assert_eq!(manager.list_workspaces().await.unwrap().len(), 1);
    // Keep the invalid bytes for diagnosis and explicit clear.
    let id = invalid.id.clone();
    let stored: String = manager
        .store
        .run(move |db| {
            Ok(db.query_row(
                "SELECT record FROM pr_cleanup WHERE space_id=?1",
                [id],
                |row| row.get(0),
            )?)
        })
        .await
        .unwrap();
    assert_eq!(stored, record);
    sweep(&manager, &mut timers).await.unwrap();
    let notifications = manager.notifications(true, 100).await.unwrap();
    let failures: Vec<_> = notifications
        .iter()
        .filter(|notification| notification.workspace.as_deref() == Some(&invalid.name))
        .collect();
    assert_eq!(
        failures.len(),
        1,
        "repeated invalid records collapse until read"
    );
    assert_eq!(failures[0].kind, NotificationKind::CleanupFailed);
}

#[tokio::test]
async fn status_names_the_first_reason_cleanup_keeps_a_workspace() {
    use crate::daemon::test_support::{commit, git, manager, repository};
    let (temp, manager) = manager().await;
    let checkout = repository(temp.path(), "repo");
    let repo = manager
        .register_repository(checkout.to_str().unwrap().into(), None, None)
        .await
        .unwrap();
    let workspace = manager
        .create_workspace(&repo.id, "kept".into(), None, None, None)
        .await
        .unwrap();
    let reason = || async { manager.cleanup_reason(&workspace).await };
    assert_eq!(reason().await, CleanupReason::Idle { minutes: 10 });

    fs::write(workspace.path.join("work"), "work").unwrap();
    assert_eq!(reason().await, CleanupReason::Dirty);
    commit(&workspace.path, "work");
    assert_eq!(reason().await, CleanupReason::Unpushed { commits: 1 });
    git(&checkout, &["merge", "--ff-only", "kept"]);

    manager
        .acquire_hold(
            &SpaceSelector::id(&workspace.id),
            "review".into(),
            None,
            crate::model::HoldPlacer::User,
        )
        .await
        .unwrap();
    let ask = crate::daemon::questions::Ask {
        question: "Anything else?".into(),
        recommendation: "No".into(),
        reason: None,
        choices: vec![],
    };
    manager
        .mark_done(
            &SpaceSelector::id(&workspace.id),
            false,
            false,
            vec![],
            &Attacher::User,
        )
        .await
        .unwrap();
    let question = manager
        .ask_question(
            &SpaceSelector::from(&workspace.name),
            ask,
            Vec::new(),
            &Attacher::User,
        )
        .await
        .unwrap();
    assert_eq!(reason().await, CleanupReason::Held);
    manager
        .release_hold(
            &SpaceSelector::id(&workspace.id),
            "review".into(),
            crate::model::HoldPlacer::User,
        )
        .await
        .unwrap();
    assert_eq!(reason().await, CleanupReason::Question);
    manager
        .answer_question(question.number, "No".into(), None)
        .await
        .unwrap();
    assert_eq!(
        reason().await,
        CleanupReason::Completed { integrated: false }
    );
    manager
        .undo_done(&SpaceSelector::id(&workspace.id))
        .await
        .unwrap();

    manager
        .write_local_config(&repo.id, Some("[cleanup.auto]\nenabled = false\n".into()))
        .await
        .unwrap();
    let status = manager
        .worktree_status(&SpaceSelector::id(&workspace.id))
        .await
        .unwrap()
        .0;
    assert_eq!(status.cleanup_reason, CleanupReason::IdleDisabled);
}

#[tokio::test]
async fn cleanup_preserves_work_and_rechecks_activity_before_deleting() {
    use crate::{
        daemon::{
            ports::PortRequest,
            resources::ResourceRequest,
            test_support::{commit, git as git_in, manager, repository},
        },
        git,
    };
    let (temp, manager) = manager().await;
    let repository_dir = repository(temp.path(), "repo");
    fs::write(repository_dir.join(".gitignore"), "ignored/\n").unwrap();
    commit(&repository_dir, ".gitignore");
    crate::daemon::test_support::configure(
        &manager,
        "[cleanup]\ngrace_minutes = 0\n[resources.test-lock]\n",
    )
    .await;
    let repo = manager
        .register_repository(repository_dir.to_str().unwrap().into(), None, None)
        .await
        .unwrap();
    let workspace = manager
        .create_workspace(&repo.id, "idle".into(), None, None, None)
        .await
        .unwrap();
    let snapshot = |manager: &Arc<Manager>| {
        let manager = manager.clone();
        let id = workspace.id.clone();
        async move { manager.cleanup_snapshot(&id).await.unwrap() }
    };
    let lease = |pool: &str, mode: Option<crate::daemon::resources::LockMode>| ResourceRequest {
        mode,
        kind: None,
        pool: pool.into(),
        name: "default".into(),
        resource: None,
        reason: None,
        queue: false,
    };
    fs::write(workspace.path.join("work"), "landed later\n").unwrap();
    commit(&workspace.path, "work");
    assert!(
        snapshot(&manager).await.is_none(),
        "unpushed work must be retained"
    );
    // Work on the local default branch is retained without any remote.
    git_in(&repository_dir, &["merge", "--ff-only", "idle"]);
    manager
        .acquire_resource(
            &SpaceSelector::id(&workspace.id),
            lease("test-lock", None),
            None,
        )
        .await
        .unwrap();
    assert!(
        snapshot(&manager).await.is_none(),
        "a resource lease must prevent automatic removal"
    );
    manager
        .release_resource(
            &SpaceSelector::id(&workspace.id),
            "test-lock".into(),
            "default".into(),
        )
        .await
        .unwrap();
    let rwlock_config = workspace.path.join(".shoal.toml");
    fs::write(&rwlock_config, "[resources.cache]\nkind='rwlock'\n").unwrap();
    for mode in [
        crate::daemon::resources::LockMode::Read,
        crate::daemon::resources::LockMode::Write,
    ] {
        manager
            .acquire_resource(
                &SpaceSelector::id(&workspace.id),
                lease("cache", Some(mode)),
                None,
            )
            .await
            .unwrap();
        // Remove config so only the lease can keep this clean/pushed worktree alive.
        fs::remove_file(&rwlock_config).unwrap();
        assert!(snapshot(&manager).await.is_none());
        manager
            .release_resource(
                &SpaceSelector::id(&workspace.id),
                "cache".into(),
                "default".into(),
            )
            .await
            .unwrap();
        fs::write(&rwlock_config, "[resources.cache]\nkind='rwlock'\n").unwrap();
    }
    fs::remove_file(&rwlock_config).unwrap();
    let original = snapshot(&manager).await.unwrap();
    manager
        .acquire_port(
            &SpaceSelector::id(&workspace.id),
            "web".into(),
            PortRequest {
                reason: Some("cleanup test".into()),
                ..Default::default()
            },
            None,
        )
        .await
        .unwrap();
    fs::write(workspace.path.join("dirty"), "retain me").unwrap();
    assert!(snapshot(&manager).await.is_none());
    assert!(
        manager
            .remove_idle(&SpaceSelector::id(&workspace.id), original)
            .await
            .is_err()
    );
    fs::remove_file(workspace.path.join("dirty")).unwrap();
    let before_command = snapshot(&manager).await.unwrap();
    let started = manager
        .begin_execution(
            &workspace.id,
            None,
            crate::daemon::workspace::ExecutionKind::Command,
            None,
        )
        .await
        .unwrap();
    assert!(snapshot(&manager).await.is_none());
    manager
        .finish_execution(
            started.plan.id,
            crate::daemon::workspace::ExecutionKind::Command,
            Some(0),
        )
        .await
        .unwrap();
    let after_command = snapshot(&manager).await.unwrap();
    assert_ne!(
        before_command, after_command,
        "short executions must reset the timer"
    );
    fs::create_dir(workspace.path.join("ignored")).unwrap();
    fs::write(workspace.path.join("ignored/build-output"), "build").unwrap();
    assert!(
        manager
            .remove_idle(&SpaceSelector::id(&workspace.id), after_command)
            .await
            .is_err(),
        "ignored file updates must reset the timer"
    );
    let final_snapshot = snapshot(&manager).await.unwrap();
    // The repository's own idle delay applies before the global default.
    manager
        .write_local_config(&repo.id, Some("[cleanup.auto]\nidle_minutes = 60\n".into()))
        .await
        .unwrap();
    let idle_for = |seconds| Idle {
        snapshot: final_snapshot,
        since: Instant::now() - Duration::from_secs(seconds),
    };
    let mut timers = Timers::default();
    timers.idle.insert(workspace.id.clone(), idle_for(600));
    sweep(&manager, &mut timers).await.unwrap();
    assert!(
        workspace.path.exists(),
        "the repository's hour outlasts the global ten minutes"
    );
    timers.idle.insert(workspace.id.clone(), idle_for(3600));
    sweep(&manager, &mut timers).await.unwrap();
    assert!(!workspace.path.exists());
    assert!(manager.list_workspaces().await.unwrap().is_empty());
    assert!(manager.list_ports(None).await.unwrap().is_empty());
    assert_eq!(
        git::run(
            &repository_dir,
            &[
                "for-each-ref",
                "--format=%(refname)",
                &format!("refs/heads/{}", workspace.branch.as_deref().unwrap())
            ],
        )
        .await
        .unwrap(),
        ""
    );
}

#[tokio::test]
async fn requested_cleanup_removes_candidates_without_idle_delay() {
    use crate::daemon::test_support::{manager, repository};
    let (temp, manager) = manager().await;
    let checkout = repository(temp.path(), "repo");
    let repo = manager
        .register_repository(checkout.to_str().unwrap().into(), None, None)
        .await
        .unwrap();
    // The request applies where idle cleanup is disabled.
    manager
        .write_local_config(&repo.id, Some("[cleanup.auto]\nenabled = false\n".into()))
        .await
        .unwrap();
    for name in ["broken", "dirty", "held", "idle"] {
        manager
            .create_workspace(&repo.id, name.into(), None, None, None)
            .await
            .unwrap();
    }
    // An inspection failure is reported rather than read as nothing to do.
    let broken = manager.workspace(&"broken".into()).await.unwrap();
    fs::remove_file(broken.path.join(".git")).unwrap();
    let dirty = manager.workspace(&"dirty".into()).await.unwrap();
    fs::write(dirty.path.join("work"), "retain me").unwrap();
    let held = manager.workspace(&"held".into()).await.unwrap();
    manager
        .acquire_hold(
            &SpaceSelector::id(&held.id),
            "keep".into(),
            None,
            crate::model::HoldPlacer::User,
        )
        .await
        .unwrap();
    let names = |manager: &Arc<Manager>| {
        let manager = manager.clone();
        async move {
            manager
                .list_workspaces()
                .await
                .unwrap()
                .into_iter()
                .map(|workspace| workspace.name)
                .collect::<Vec<_>>()
        }
    };

    let retained = |report: &ManualCleanup| {
        report
            .retained
            .iter()
            .map(|retained| retained.space.clone())
            .collect::<Vec<_>>()
    };

    let all = Selection {
        spaces: Pick::All,
        ..Selection::default()
    };
    let preview = on_request(&manager, true, &all).await.unwrap();
    assert_eq!(preview.removed, ["idle"]);
    assert_eq!(retained(&preview), ["broken"]);
    let kept = preview
        .kept
        .iter()
        .map(|kept| (kept.space.as_str(), kept.reason.clone()))
        .collect::<Vec<_>>();
    assert_eq!(
        kept,
        [
            ("dirty", CleanupReason::Dirty),
            ("held", CleanupReason::Held)
        ]
    );
    assert_eq!(names(&manager).await, ["broken", "dirty", "held", "idle"]);

    // Named workspaces say why they stay; others are left out.
    let named = Selection {
        spaces: Pick::Only(vec!["dirty".into(), "held".into()]),
        ..Selection::default()
    };
    let report = on_request(&manager, false, &named).await.unwrap();
    assert!(report.removed.is_empty());
    assert_eq!(
        report
            .kept
            .iter()
            .map(|kept| kept.space.as_str())
            .collect::<Vec<_>>(),
        ["dirty", "held"]
    );
    let unknown = Selection {
        spaces: Pick::Only(vec!["missing".into()]),
        ..Selection::default()
    };
    assert!(on_request(&manager, false, &unknown).await.is_err());
    assert_eq!(names(&manager).await, ["broken", "dirty", "held", "idle"]);

    let report = on_request(&manager, false, &all).await.unwrap();
    assert!(!report.dry_run);
    assert_eq!(report.removed, ["idle"]);
    assert_eq!(retained(&report), ["broken"]);
    assert!(
        report.kept.is_empty(),
        "only a dry run lists kept workspaces"
    );
    assert_eq!(names(&manager).await, ["broken", "dirty", "held"]);
}

#[tokio::test]
async fn holds_block_idle_sweeps_and_release_restores_eligibility() {
    use crate::daemon::test_support::{git, manager, repository};
    let (temp, manager) = manager().await;
    let checkout = repository(temp.path(), "repo");
    let repo = manager
        .register_repository(checkout.to_str().unwrap().into(), None, None)
        .await
        .unwrap();
    let workspace = manager
        .create_workspace(&repo.id, "held".into(), None, None, None)
        .await
        .unwrap();
    let snapshot = manager
        .cleanup_snapshot(&workspace.id)
        .await
        .unwrap()
        .unwrap();
    let mut timers = Timers::default();
    timers.idle.insert(
        workspace.id.clone(),
        Idle {
            snapshot,
            since: Instant::now() - Duration::from_secs(3600),
        },
    );
    for name in ["app-one", "app-two"] {
        manager
            .acquire_hold(
                &SpaceSelector::id(&workspace.id),
                name.into(),
                None,
                crate::model::HoldPlacer::User,
            )
            .await
            .unwrap();
    }
    sweep(&manager, &mut timers).await.unwrap();
    assert!(workspace.path.exists());
    assert!(!git(&checkout, &["branch", "--list", "held"]).is_empty());
    assert!(!timers.idle.contains_key(&workspace.id));
    manager
        .release_hold(
            &SpaceSelector::id(&workspace.id),
            "app-one".into(),
            crate::model::HoldPlacer::User,
        )
        .await
        .unwrap();
    sweep(&manager, &mut timers).await.unwrap();
    assert!(
        manager
            .cleanup_snapshot(&workspace.id)
            .await
            .unwrap()
            .is_none()
    );
    manager
        .release_hold(
            &SpaceSelector::id(&workspace.id),
            "app-two".into(),
            crate::model::HoldPlacer::User,
        )
        .await
        .unwrap();
    let snapshot = manager
        .cleanup_snapshot(&workspace.id)
        .await
        .unwrap()
        .unwrap();
    timers.idle.insert(
        workspace.id.clone(),
        Idle {
            snapshot,
            since: Instant::now() - Duration::from_secs(3600),
        },
    );
    sweep(&manager, &mut timers).await.unwrap();
    assert!(!workspace.path.exists());
    assert!(git(&checkout, &["branch", "--list", "held"]).is_empty());
    assert!(!manager.has_holds(&workspace.id).await.unwrap());
}

#[tokio::test]
async fn ignored_changes_during_idle_delay_restart_timer_at_expiry() {
    use crate::daemon::test_support::{commit, manager, repository};
    let (temp, manager) = manager().await;
    let checkout = repository(temp.path(), "repo");
    fs::write(checkout.join(".gitignore"), "ignored/\n").unwrap();
    commit(&checkout, ".gitignore");
    let repo = manager
        .register_repository(checkout.to_str().unwrap().into(), None, None)
        .await
        .unwrap();
    let workspace = manager
        .create_workspace(&repo.id, "idle".into(), None, None, None)
        .await
        .unwrap();
    let mut timers = Timers::default();
    let expire = |timers: &mut Timers| {
        timers.idle.get_mut(&workspace.id).unwrap().since -= Duration::from_secs(600);
    };
    sweep(&manager, &mut timers).await.unwrap();
    let started = timers.idle[&workspace.id].snapshot;
    fs::create_dir(workspace.path.join("ignored")).unwrap();
    fs::write(workspace.path.join("ignored/build-output"), "build").unwrap();
    // Passes before expiry compare only HEAD and activity.
    sweep(&manager, &mut timers).await.unwrap();
    assert_eq!(timers.idle[&workspace.id].snapshot, started);
    expire(&mut timers);
    sweep(&manager, &mut timers).await.unwrap();
    assert!(
        workspace.path.exists(),
        "the ignored change restarts the timer"
    );
    assert_ne!(timers.idle[&workspace.id].snapshot, started);
    expire(&mut timers);
    sweep(&manager, &mut timers).await.unwrap();
    assert!(!workspace.path.exists());
}

#[tokio::test]
async fn git_state_is_inspected_when_the_idle_timer_starts_and_expires() {
    use crate::daemon::test_support::{commit, git, manager, repository};
    let (temp, manager) = manager().await;
    let checkout = repository(temp.path(), "repo");
    fs::write(checkout.join("second"), "second\n").unwrap();
    commit(&checkout, "second");
    let repo = manager
        .register_repository(checkout.to_str().unwrap().into(), None, None)
        .await
        .unwrap();
    let workspace = manager
        .create_workspace(&repo.id, "idle".into(), None, None, None)
        .await
        .unwrap();
    let mut timers = Timers::default();
    sweep(&manager, &mut timers).await.unwrap();
    let started = timers.idle[&workspace.id].snapshot;
    // The workspace's last commit is no longer anywhere else, which changes
    // neither its HEAD nor its files.
    git(&checkout, &["reset", "--hard", "HEAD~1"]);
    sweep(&manager, &mut timers).await.unwrap();
    assert_eq!(timers.idle[&workspace.id].snapshot, started);
    timers.idle.get_mut(&workspace.id).unwrap().since -= Duration::from_secs(600);
    sweep(&manager, &mut timers).await.unwrap();
    assert!(workspace.path.exists());
    assert!(!timers.idle.contains_key(&workspace.id));
    assert_eq!(
        manager.cleanup_reason(&workspace).await,
        CleanupReason::Unpushed { commits: 1 }
    );
}

#[tokio::test]
async fn a_process_in_the_worktree_stops_a_running_idle_timer() {
    use crate::daemon::test_support::{manager, repository};
    let (temp, manager) = manager().await;
    let checkout = repository(temp.path(), "repo");
    let repo = manager
        .register_repository(checkout.to_str().unwrap().into(), None, None)
        .await
        .unwrap();
    let workspace = manager
        .create_workspace(&repo.id, "idle".into(), None, None, None)
        .await
        .unwrap();
    let mut timers = Timers::default();
    sweep(&manager, &mut timers).await.unwrap();
    assert!(timers.idle.contains_key(&workspace.id));
    let mut child = tokio::process::Command::new("sleep")
        .arg("60")
        .current_dir(&workspace.path)
        .kill_on_drop(true)
        .spawn()
        .unwrap();
    sweep(&manager, &mut timers).await.unwrap();
    assert!(!timers.idle.contains_key(&workspace.id));
    child.kill().await.unwrap();
}

#[tokio::test]
async fn a_sweep_skips_workspaces_where_file_access_stalled_and_cleans_up_the_others() {
    use crate::daemon::{
        links::pr::Action,
        test_support::{manager, repository},
    };
    let (temp, manager) = manager().await;
    let mut timers = Timers::default();
    let mut created = HashMap::new();
    // Each repository's workspaces share a directory, as on one volume.
    for name in ["answers", "stalled"] {
        let checkout = repository(temp.path(), name);
        let repo = manager
            .register_repository(checkout.to_str().unwrap().into(), None, None)
            .await
            .unwrap();
        let create = |kind: &str| {
            manager.create_workspace(&repo.id, format!("{name}-{kind}"), None, None, None)
        };
        let done = create("done").await.unwrap();
        manager
            .mark_done(
                &SpaceSelector::id(&done.id),
                true,
                false,
                vec![],
                &Attacher::User,
            )
            .await
            .unwrap();
        let merged = create("merged").await.unwrap();
        manager
            .set_pr(&SpaceSelector::id(&merged.id), Action::Acknowledge)
            .await
            .unwrap();
        let idle = create("idle").await.unwrap();
        timers.idle.insert(
            idle.id.clone(),
            Idle {
                snapshot: manager.cleanup_snapshot(&idle.id).await.unwrap().unwrap(),
                since: Instant::now() - Duration::from_secs(3600),
            },
        );
        created.insert(name, [done, merged, idle]);
    }
    manager
        .file_access
        .stall_rechecking(&created["stalled"][0].path);

    sweep(&manager, &mut timers).await.unwrap();

    for workspace in &created["answers"] {
        assert!(
            !workspace.path.exists(),
            "the sweep removes {}",
            workspace.name
        );
    }
    for workspace in &created["stalled"] {
        assert!(
            workspace.path.exists(),
            "the sweep keeps {}",
            workspace.name
        );
    }
    let mut remaining: Vec<_> = manager
        .list_workspaces()
        .await
        .unwrap()
        .into_iter()
        .map(|workspace| workspace.name)
        .collect();
    remaining.sort();
    assert_eq!(
        remaining,
        ["stalled-done", "stalled-idle", "stalled-merged"]
    );
    // Skipped rather than failed, so no cleanup failure is reported.
    let failures: Vec<_> = manager
        .notifications(true, 100)
        .await
        .unwrap()
        .into_iter()
        .filter(|notification| notification.kind == NotificationKind::CleanupFailed)
        .map(|notification| notification.message)
        .collect();
    assert_eq!(failures, Vec::<String>::new());
}

#[tokio::test]
async fn a_sweep_rechecks_a_stalled_worktree_deleted_since_and_forgets_it() {
    use crate::daemon::test_support::{manager, repository};
    let (temp, manager) = manager().await;
    let checkout = repository(temp.path(), "repo");
    let repo = manager
        .register_repository(checkout.to_str().unwrap().into(), None, None)
        .await
        .unwrap();
    let workspace = manager
        .create_workspace(&repo.id, "deleted".into(), None, None, None)
        .await
        .unwrap();
    // Paused only around the stall, since real Git would let the clock run
    // out its time limit.
    tokio::time::pause();
    let stalled = manager
        .check_access(&workspace.path, std::future::pending::<Result<()>>())
        .await;
    tokio::time::advance(file_access::RECHECK_AFTER).await;
    tokio::time::resume();
    assert!(stalled.is_err());
    // Branch observation reads only worktrees that are there.
    fs::remove_dir_all(&workspace.path).unwrap();

    sweep(&manager, &mut Timers::default()).await.unwrap();

    assert_eq!(manager.file_access.stalls(), []);
    assert!(manager.list_workspaces().await.unwrap().is_empty());
}

#[tokio::test]
async fn completion_cleanup_keeps_a_parent_whose_child_elsewhere_stalled() {
    use crate::daemon::test_support::{manager, repository};
    let (temp, manager) = manager().await;
    let checkout = repository(temp.path(), "repo");
    let repo = manager
        .register_repository(checkout.to_str().unwrap().into(), None, None)
        .await
        .unwrap();
    // The parent's explicit path puts it in another directory than its child.
    let parent = manager
        .create_workspace(
            &repo.id,
            "parent".into(),
            None,
            None,
            Some(temp.path().join("elsewhere").join("parent")),
        )
        .await
        .unwrap();
    let child = manager
        .split_workspace(
            &SpaceSelector::id(&parent.id),
            "child".into(),
            None,
            Default::default(),
        )
        .await
        .unwrap();
    assert_ne!(parent.path.parent(), child.path.parent());
    manager
        .mark_done(
            &SpaceSelector::id(&parent.id),
            true,
            false,
            vec![],
            &Attacher::User,
        )
        .await
        .unwrap();
    manager.file_access.stall_rechecking(&child.path);

    sweep(&manager, &mut Timers::default()).await.unwrap();

    let failures: Vec<_> = manager
        .notifications(true, 100)
        .await
        .unwrap()
        .into_iter()
        .filter(|notification| notification.kind == NotificationKind::CleanupFailed)
        .map(|notification| notification.workspace)
        .collect();
    assert_eq!(failures, [Some("parent".to_owned())]);
    assert!(parent.path.exists());
    assert!(child.path.exists());
}

/// A registered repository, a parent at an explicit path in another directory
/// than the repository's workspaces, and a workspace there built on it.
async fn split_across_directories(
    base: bool,
) -> (tempfile::TempDir, Arc<Manager>, Workspace, Workspace) {
    use crate::daemon::test_support::{manager, repository};
    let (temp, manager) = manager().await;
    let checkout = repository(temp.path(), "repo");
    let repo = manager
        .register_repository(checkout.to_str().unwrap().into(), None, None)
        .await
        .unwrap();
    let parent = manager
        .create_workspace(
            &repo.id,
            "parent".into(),
            None,
            None,
            Some(temp.path().join("elsewhere").join("parent")),
        )
        .await
        .unwrap();
    let below = if base {
        manager
            .create_workspace(
                &repo.id,
                "stacked".into(),
                Some("parent".into()),
                None,
                None,
            )
            .await
    } else {
        manager
            .split_workspace(
                &SpaceSelector::id(&parent.id),
                "child".into(),
                None,
                Default::default(),
            )
            .await
    }
    .unwrap();
    assert_ne!(parent.path.parent(), below.path.parent());
    (temp, manager, parent, below)
}

#[tokio::test]
async fn deleted_parent_cleanup_keeps_a_parent_whose_child_elsewhere_stalled() {
    let (_temp, manager, parent, child) = split_across_directories(false).await;
    fs::remove_dir_all(&parent.path).unwrap();
    manager.file_access.stall_rechecking(&child.path);

    sweep(&manager, &mut Timers::default()).await.unwrap();

    let remaining: Vec<_> = manager
        .list_workspaces()
        .await
        .unwrap()
        .into_iter()
        .map(|workspace| workspace.name)
        .collect();
    assert_eq!(remaining.len(), 2, "{remaining:?}");
    assert!(child.path.exists());
}

#[tokio::test]
async fn pr_cleanup_skips_a_base_whose_stacked_workspace_elsewhere_stalled() {
    let (_temp, manager, base, stacked) = split_across_directories(true).await;
    let id = base.id.clone();
    let registration = serde_json::json!({ "url": "https://example.invalid/o/r/pulls/1" });
    manager
        .store
        .run(move |db| {
            db.execute(
                "INSERT INTO pr_cleanup(space_id,record) VALUES (?1,?2)",
                rusqlite::params![id, registration.to_string()],
            )?;
            Ok(())
        })
        .await
        .unwrap();
    manager.file_access.stall_rechecking(&stacked.path);

    manager.sweep_prs().await.unwrap();

    // Restacking never looked the PR up, so no lookup failure is recorded.
    let registration = manager.pr_registration(&base.id).await.unwrap().unwrap();
    assert_eq!(registration.error, None);
}

#[tokio::test]
async fn a_child_keeps_while_its_completed_parent_elsewhere_stalled() {
    let (_temp, manager, parent, child) = split_across_directories(false).await;
    manager
        .mark_done(
            &SpaceSelector::id(&parent.id),
            true,
            false,
            vec![],
            &Attacher::User,
        )
        .await
        .unwrap();
    manager.file_access.stall_rechecking(&parent.path);

    sweep(&manager, &mut Timers::default()).await.unwrap();

    // The parent's completion cannot be read, so it takes neither.
    assert!(parent.path.exists());
    assert!(child.path.exists());
}

#[tokio::test]
async fn pr_cleanup_passes_over_a_stacked_workspace_it_removed_earlier() {
    use crate::daemon::{
        links::pr::Action,
        test_support::{manager, repository},
    };
    let (temp, manager) = manager().await;
    let checkout = repository(temp.path(), "repo");
    let repo = manager
        .register_repository(checkout.to_str().unwrap().into(), None, None)
        .await
        .unwrap();
    manager
        .create_workspace(&repo.id, "base".into(), None, None, None)
        .await
        .unwrap();
    // Its name puts it before its base in the sweep, which still lists it
    // on the base after removing it.
    let stacked = manager
        .create_workspace(
            &repo.id,
            "a-stacked".into(),
            Some("base".into()),
            None,
            None,
        )
        .await
        .unwrap();
    manager
        .set_pr(&SpaceSelector::id(&stacked.id), Action::Acknowledge)
        .await
        .unwrap();

    manager.sweep_prs().await.unwrap();

    assert!(!stacked.path.exists());
}

/// A chat with a workspace below it, whose agent `process` runs.
async fn chat_with_workspace(
    temp: &tempfile::TempDir,
    manager: &Manager,
    process: &crate::daemon::test_support::ProcessGroup,
) -> (crate::model::Chat, Workspace) {
    let checkout = crate::daemon::test_support::repository(temp.path(), "repo");
    let repo = manager
        .register_repository(checkout.to_str().unwrap().into(), None, None)
        .await
        .unwrap();
    let chat = manager
        .start_chat(
            repo.id.clone().into(),
            "talk".into(),
            "claude".into(),
            process.id(),
            None,
            false,
        )
        .await
        .unwrap();
    let opened = manager
        .create_workspace_below(
            repo.id.into(),
            "opened".into(),
            None,
            Default::default(),
            Some(chat.id.clone()),
        )
        .await
        .unwrap();
    (chat, opened)
}

#[tokio::test]
async fn idle_cleanup_removes_a_stopped_chat_with_the_workspaces_below_it() {
    let (temp, manager) = crate::daemon::test_support::manager().await;
    let process = crate::daemon::test_support::ProcessGroup::spawn(
        std::process::Command::new("sleep").arg("600"),
    );
    let (chat, opened) = chat_with_workspace(&temp, &manager, &process).await;
    // A running chat is in use.
    assert_eq!(
        manager
            .chat_cleanup_snapshot(&chat, Rule::Idle)
            .await
            .unwrap(),
        None
    );
    process.kill();
    let stopped = manager.chat_by_id(&chat.id).await.unwrap().unwrap();
    let snapshot = (manager.chat_cleanup_snapshot(&stopped, Rule::Idle))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(snapshot.tree, 0);
    // A restore and a new stop between two passes is another snapshot.
    let id = chat.id.clone();
    manager
        .store
        .run(move |db| {
            db.execute("UPDATE chats SET stopped_at=stopped_at+1 WHERE id=?1", [id])?;
            Ok(())
        })
        .await
        .unwrap();
    let restarted = (manager.chat_cleanup_snapshot(&stopped, Rule::Idle))
        .await
        .unwrap()
        .unwrap();
    assert_ne!(restarted, snapshot);
    let snapshot = restarted;
    let mut timers = Timers::default();
    timers.idle.insert(
        chat.id.clone(),
        Idle {
            snapshot,
            since: Instant::now() - Duration::from_secs(3600),
        },
    );
    sweep(&manager, &mut timers).await.unwrap();
    assert!(manager.chats().await.unwrap().is_empty());
    assert!(manager.workspace_by_id(&opened.id).await.is_err());
    assert!(!opened.path.exists());
}

#[tokio::test]
async fn a_chat_shoal_saved_or_with_assignment_state_is_kept_from_idle_cleanup() {
    let (temp, manager) = crate::daemon::test_support::manager().await;
    let process = crate::daemon::test_support::ProcessGroup::spawn(
        std::process::Command::new("sleep").arg("600"),
    );
    let (chat, _) = chat_with_workspace(&temp, &manager, &process).await;
    manager
        .mark_chat_done(&chat.id, false, Vec::new(), &Attacher::User)
        .await
        .unwrap();
    // A save to come back on its own keeps it, as a queued agent does.
    manager
        .save_chats(crate::state::ChatRestore::Automatic)
        .await;
    let saved = manager.chat_by_id(&chat.id).await.unwrap().unwrap();
    assert_eq!(
        manager
            .chat_cleanup_snapshot(&saved, Rule::Idle)
            .await
            .unwrap(),
        None
    );
    // Saved for `shoal resume`, it is kept as a workspace's saved agent
    // keeps it.
    let id = chat.id.clone();
    manager
        .store
        .run(move |db| {
            db.execute("UPDATE chats SET restore='resume' WHERE id=?1", [id])?;
            Ok(())
        })
        .await
        .unwrap();
    let stopped = manager.chat_by_id(&chat.id).await.unwrap().unwrap();
    let idle = async || {
        (manager.chat_cleanup_candidacy(&stopped, Rule::Idle))
            .await
            .unwrap()
            .unwrap_err()
    };
    assert_eq!(idle().await, CleanupReason::Recovering);
    // Once its agent exited instead, its completion still keeps it from
    // idle cleanup, which merged cleanup disregards.
    let id = chat.id.clone();
    manager
        .store
        .run(move |db| {
            db.execute("UPDATE chats SET saved=0 WHERE id=?1", [id])?;
            Ok(())
        })
        .await
        .unwrap();
    assert_eq!(idle().await, CleanupReason::Completed { integrated: false });
    assert!(
        manager
            .chat_cleanup_snapshot(&stopped, Rule::Merged)
            .await
            .unwrap()
            .is_some()
    );
}

#[tokio::test]
async fn completion_cleanup_removes_a_done_chat_whatever_prs_it_watches() {
    let (temp, manager) = crate::daemon::test_support::manager().await;
    let process = crate::daemon::test_support::ProcessGroup::spawn(
        std::process::Command::new("sleep").arg("600"),
    );
    let (chat, opened) = chat_with_workspace(&temp, &manager, &process).await;
    let record = serde_json::json!({
        "kind": "watch",
        "urls": ["https://github.com/team/repo/pull/1"],
        "error": null,
    })
    .to_string();
    let id = chat.id.clone();
    manager
        .store
        .run(move |db| {
            db.execute(
                "INSERT INTO pr_cleanup(space_id,record) VALUES (?1,?2)",
                rusqlite::params![id, record],
            )?;
            Ok(())
        })
        .await
        .unwrap();
    assert!(manager.pr_registration(&chat.id).await.unwrap().is_some());
    manager
        .mark_chat_done(&chat.id, true, Vec::new(), &Attacher::User)
        .await
        .unwrap();

    manager.sweep_completed().await.unwrap();
    assert!(manager.chats().await.unwrap().is_empty());
    assert!(manager.workspace_by_id(&opened.id).await.is_err());
    assert!(!process_wait(process).success());
}

/// How `process` exited.
fn process_wait(
    mut process: crate::daemon::test_support::ProcessGroup,
) -> std::process::ExitStatus {
    process.wait().unwrap()
}

#[tokio::test]
async fn a_stopped_chat_whose_processes_run_on_starts_no_grace_period() {
    let (temp, manager) = crate::daemon::test_support::manager().await;
    let process = crate::daemon::test_support::ProcessGroup::spawn(
        std::process::Command::new("sleep").arg("600"),
    );
    let (chat, opened) = chat_with_workspace(&temp, &manager, &process).await;
    // As a stop that never ended the agent left it.
    let id = chat.id.clone();
    manager
        .store
        .run(move |db| {
            db.execute(
                "UPDATE chats SET restore='resume',stopped_at=0 WHERE id=?1",
                [id],
            )?;
            Ok(())
        })
        .await
        .unwrap();
    let stopped = manager.chat_by_id(&chat.id).await.unwrap().unwrap();
    let snapshot = (manager.chat_cleanup_snapshot(&stopped, Rule::Merged))
        .await
        .unwrap()
        .unwrap();
    let removed = manager
        .remove_unused_chat(&stopped, snapshot, EventCause::Merged)
        .await;
    assert!(removed.is_err());
    assert_eq!(manager.cleanup_at(&chat.id).await.unwrap(), None);
    assert!(manager.workspace_by_id(&opened.id).await.is_ok());
}

#[tokio::test]
async fn a_stalled_directory_below_a_done_chat_starts_no_grace_period() {
    let (temp, manager) = crate::daemon::test_support::manager().await;
    let process = crate::daemon::test_support::ProcessGroup::spawn(
        std::process::Command::new("sleep").arg("600"),
    );
    let (chat, opened) = chat_with_workspace(&temp, &manager, &process).await;
    manager
        .write_local_config(
            &opened.repository_id,
            Some("[cleanup]\ngrace_minutes = 600\n".into()),
        )
        .await
        .unwrap();
    let completion = manager
        .mark_chat_done(&chat.id, true, Vec::new(), &Attacher::User)
        .await
        .unwrap();
    let chat = manager.chat_by_id(&chat.id).await.unwrap().unwrap();
    manager.file_access.stall_rechecking(&opened.path);
    let removed = manager.remove_completed_chat(&chat, &completion).await;
    assert!(removed.is_err());
    assert_eq!(manager.cleanup_at(&chat.id).await.unwrap(), None);
    // Once access recovers, the grace period starts, and only then runs.
    manager.file_access.recover_at(&opened.path);
    assert!(
        !manager
            .remove_completed_chat(&chat, &completion)
            .await
            .unwrap()
    );
    assert!(manager.cleanup_at(&chat.id).await.unwrap().is_some());
    assert!(manager.workspace_by_id(&opened.id).await.is_ok());
}

#[tokio::test]
async fn a_stopped_chat_withdraws_its_completion_and_stays() {
    let (temp, manager) = crate::daemon::test_support::manager().await;
    let process = crate::daemon::test_support::ProcessGroup::spawn(
        std::process::Command::new("sleep").arg("600"),
    );
    let (chat, opened) = chat_with_workspace(&temp, &manager, &process).await;
    manager
        .mark_chat_done(&chat.id, true, Vec::new(), &Attacher::User)
        .await
        .unwrap();
    process.kill();
    manager.stop_finished_chats().await.unwrap();

    let withdrawn = manager
        .undo_space_done(&SpaceSelector::from(&chat.name))
        .await
        .unwrap();
    assert!(withdrawn.cleanup);
    manager.sweep_completed().await.unwrap();
    let stopped = manager.chat_by_id(&chat.id).await.unwrap().unwrap();
    assert_eq!(stopped.completion, None);
    assert!(manager.workspace_by_id(&opened.id).await.is_ok());
}

#[tokio::test]
async fn a_done_chat_something_keeps_still_takes_the_workspaces_below_it() {
    let (temp, manager) = crate::daemon::test_support::manager().await;
    let process = crate::daemon::test_support::ProcessGroup::spawn(
        std::process::Command::new("sleep").arg("600"),
    );
    let (chat, opened) = chat_with_workspace(&temp, &manager, &process).await;
    manager
        .mark_chat_done(&chat.id, true, Vec::new(), &Attacher::User)
        .await
        .unwrap();
    // A question asked after the completion keeps the chat.
    let ask = crate::daemon::questions::Ask {
        question: "Which database?".into(),
        recommendation: "SQLite".into(),
        reason: None,
        choices: Vec::new(),
    };
    manager
        .ask_question(
            &SpaceSelector::from(&chat.name),
            ask,
            Vec::new(),
            &Attacher::User,
        )
        .await
        .unwrap();

    manager.sweep_completed().await.unwrap();
    assert!(manager.chat_by_id(&chat.id).await.unwrap().is_some());
    assert!(manager.workspace_by_id(&opened.id).await.is_err());
    assert!(!opened.path.exists());
}

#[tokio::test]
async fn a_chats_open_question_keeps_it_from_every_unattended_removal() {
    let (temp, manager) = crate::daemon::test_support::manager().await;
    let process = crate::daemon::test_support::ProcessGroup::spawn(
        std::process::Command::new("sleep").arg("600"),
    );
    let (chat, _) = chat_with_workspace(&temp, &manager, &process).await;
    let ask = crate::daemon::questions::Ask {
        question: "Which database?".into(),
        recommendation: "SQLite".into(),
        reason: None,
        choices: Vec::new(),
    };
    manager
        .ask_question(
            &SpaceSelector::from(&chat.name),
            ask,
            Vec::new(),
            &Attacher::User,
        )
        .await
        .unwrap();
    process.kill();
    let stopped = manager.chat_by_id(&chat.id).await.unwrap().unwrap();
    for rule in [Rule::Idle, Rule::Merged] {
        assert_eq!(
            (manager.chat_cleanup_candidacy(&stopped, rule))
                .await
                .unwrap()
                .unwrap_err(),
            CleanupReason::Question
        );
    }
}

#[tokio::test]
async fn a_link_made_while_idle_cleanup_starts_keeps_the_chat_and_its_workspaces() {
    let (temp, manager) = crate::daemon::test_support::manager().await;
    let process = crate::daemon::test_support::ProcessGroup::spawn(
        std::process::Command::new("sleep").arg("600"),
    );
    let (chat, opened) = chat_with_workspace(&temp, &manager, &process).await;
    process.kill();
    let stopped = manager.chat_by_id(&chat.id).await.unwrap().unwrap();
    let snapshot = (manager.chat_cleanup_snapshot(&stopped, Rule::Idle))
        .await
        .unwrap()
        .unwrap();
    // A link under way holds the chat's PR gate when the removal starts...
    let gate = manager.gates.lock_prs([&chat.id]).await;
    let removal = tokio::spawn({
        let (manager, stopped) = (manager.clone(), stopped.clone());
        async move { (manager.remove_unused_chat(&stopped, snapshot, EventCause::Idle)).await }
    });
    // ...and records the issue before it lets the removal go on.
    let id = chat.id.clone();
    manager
        .store
        .run(move |db| {
            db.execute(
                "INSERT INTO space_issue(space_id,url) VALUES (?1,'https://forge/o/r/issues/1')",
                [id],
            )?;
            Ok(())
        })
        .await
        .unwrap();
    drop(gate);

    assert!(removal.await.unwrap().is_err());
    assert!(manager.chat_by_id(&chat.id).await.unwrap().is_some());
    assert!(manager.workspace_by_id(&opened.id).await.is_ok());
}

#[tokio::test]
async fn a_chat_in_a_workspace_keeps_it_while_it_runs_and_from_idle_cleanup_while_saved() {
    let (temp, manager) = crate::daemon::test_support::manager().await;
    let checkout = crate::daemon::test_support::repository(temp.path(), "repo");
    let repo = manager
        .register_repository(checkout.to_str().unwrap().into(), None, None)
        .await
        .unwrap();
    let workspace = manager
        .create_workspace(&repo.id, "kept".into(), None, None, None)
        .await
        .unwrap();
    let reason = || async { manager.cleanup_reason(&workspace).await };
    let merged = || async { manager.merged_snapshot(&workspace.id).await.unwrap() };
    assert!(merged().await.is_some());
    let process = crate::daemon::test_support::ProcessGroup::spawn(
        std::process::Command::new("sleep").arg("600"),
    );
    let chat = manager
        .start_chat_in_workspace(
            &SpaceSelector::id(&workspace.id),
            "kept-chat".into(),
            "claude".into(),
            process.id(),
            None,
            false,
        )
        .await
        .unwrap();
    // A running chat keeps it from every unattended removal.
    assert_eq!(reason().await, CleanupReason::ChatRunning);
    assert!(merged().await.is_none());

    // A chat Shoal saved keeps it from idle cleanup, as a saved agent does,
    // but not from merged cleanup.
    manager.save_chats(crate::state::ChatRestore::Resume).await;
    assert_eq!(reason().await, CleanupReason::Recovering);
    assert!(merged().await.is_some());

    // One whose agent exited keeps it from neither.
    let id = chat.id.clone();
    manager
        .store
        .run(move |db| {
            db.execute("UPDATE chats SET saved=0 WHERE id=?1", [id])?;
            Ok(())
        })
        .await
        .unwrap();
    assert_eq!(reason().await, CleanupReason::Idle { minutes: 10 });
    drop(process);
}
