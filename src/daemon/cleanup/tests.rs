use super::*;

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
    let before = fingerprint(root.path()).unwrap();
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
    assert_ne!(fingerprint(root.path()).unwrap(), before);
}

#[test]
fn health_reports_failing_and_stalled_cleanup() {
    let mut health = Health::default();
    let (start, unix) = (health.started.at, health.started.unix);
    let after = |seconds| start + Duration::from_secs(seconds);
    // Wall time runs on while the Mac sleeps; the monotonic clock does not.
    let moment = |seconds, slept| Moment {
        at: after(seconds),
        unix: unix + seconds + slept,
    };
    assert_eq!(health.problem(start), None);
    let stalled = start + STALLED_AFTER;
    assert!(
        health
            .problem(stalled)
            .unwrap()
            .contains("since the daemon started")
    );

    assert_eq!(health.record(moment(30, 0), &Ok(())), None);
    assert_eq!(health.record(moment(60, 3600), &Ok(())), None);
    assert_eq!(health.problem(after(90)), None);

    let failed = Err(anyhow::anyhow!("completion cleanup: disk is full"));
    assert_eq!(
        health.record(moment(90, 3600), &failed).as_deref(),
        Some("completion cleanup: disk is full")
    );
    assert_eq!(
        health.record(moment(120, 3600), &failed).as_deref(),
        Some("completion cleanup: disk is full"),
        "an unrecorded notification is retried"
    );
    health.failure.as_mut().unwrap().notified = true;
    assert_eq!(health.record(moment(150, 3600), &failed), None);
    let problem = health.problem(after(150)).unwrap();
    assert!(problem.contains("failed 3 consecutive passes"), "{problem}");
    assert!(problem.ends_with("completion cleanup: disk is full"));

    assert_eq!(health.record(moment(180, 3600), &Ok(())), None);
    assert_eq!(health.problem(after(180)), None);
    assert!(health.problem(after(180) + STALLED_AFTER).is_some());
}

#[tokio::test]
async fn failing_passes_notify_once_and_show_in_status() {
    use crate::test_support::{manager, repository};
    let (temp, manager) = manager().await;
    let checkout = repository(temp.path(), "repo");
    let repo = manager
        .register_repository(checkout.to_str().unwrap().into(), None, None)
        .await
        .unwrap();
    let workspace = manager
        .create_workspace(&repo.id, "done".into(), None, None, None)
        .await
        .unwrap();
    for _ in 0..2 {
        manager
            .record_sweep(Err(anyhow::anyhow!("completion cleanup: disk is full")))
            .await;
    }
    let notifications = manager.notifications(true, 100).await.unwrap();
    assert_eq!(notifications.len(), 1);
    assert_eq!(notifications[0].workspace, None);
    assert_eq!(notifications[0].kind, NotificationKind::CleanupFailed);
    assert_eq!(
        notifications[0].message,
        "automatic cleanup failed: completion cleanup: disk is full"
    );
    let status = manager.workspace_status(&workspace.id).await.unwrap();
    assert!(
        status
            .cleanup_error
            .unwrap()
            .contains("failed 2 consecutive passes since")
    );

    manager.record_sweep(Ok(())).await;
    let status = manager.workspace_status(&workspace.id).await.unwrap();
    assert_eq!(status.cleanup_error, None);
}

#[tokio::test]
async fn deleted_parent_sweep_releases_leases_and_keeps_a_branch_with_work() {
    use crate::{
        daemon::{ports::PortRequest, resources::ResourceRequest},
        test_support::{git, manager, repository},
    };
    for pending_rename in [false, true] {
        let (temp, manager) = manager().await;
        let checkout = repository(temp.path(), "repo");
        fs::write(checkout.join(".shoal.toml"), "[resources.lock]\n").unwrap();
        crate::test_support::commit(&checkout, ".shoal.toml");
        let repo = manager
            .register_repository(checkout.to_str().unwrap().into(), None, None)
            .await
            .unwrap();
        let workspace = manager
            .create_workspace(&repo.id, "deleted-parent".into(), None, None, None)
            .await
            .unwrap();
        manager
            .acquire_port(&workspace.id, "web".into(), PortRequest::default(), None)
            .await
            .unwrap();
        manager
            .acquire_resource(
                &workspace.id,
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
        crate::test_support::commit(&workspace.path, "work");
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

        assert!(manager.workspace(&workspace.id).await.is_err());
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
async fn failed_step_does_not_skip_later_steps_and_is_named() {
    use crate::test_support::{manager, repository};
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
        .mark_done(&done.id, true, false, vec![])
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
    let error = sweep(&manager, &mut timers).await.unwrap_err().to_string();
    assert!(
        error.starts_with("completion cleanup: ") && error.contains("disk is full"),
        "{error}"
    );
    assert!(!idle.path.exists());
    assert!(done.path.exists());
}

#[tokio::test]
async fn invalid_pr_record_does_not_block_other_workspace_cleanup() {
    use crate::{
        daemon::links::pr::Action,
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
        .set_pr(&merged.id, Action::Acknowledge)
        .await
        .unwrap();
    let id = invalid.id.clone();
    let record = r#"{"url":null,"head":null,"error":null}"#;
    manager
        .store
        .run(move |db| {
            db.execute(
                "INSERT INTO pr_cleanup(workspace_id,record) VALUES (?1,?2)",
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
                "SELECT record FROM pr_cleanup WHERE workspace_id=?1",
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
    assert!(
        failures[0]
            .message
            .contains("expected exactly one of url or head")
    );
}

#[tokio::test]
async fn status_names_the_first_reason_cleanup_keeps_a_workspace() {
    use crate::test_support::{commit, git, manager, repository};
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
        .acquire_hold(&workspace.id, "review".into(), None)
        .await
        .unwrap();
    let ask = crate::daemon::questions::Ask {
        question: "Anything else?".into(),
        recommendation: "No".into(),
        reason: None,
        choices: vec![],
    };
    manager
        .mark_done(&workspace.id, false, false, vec![])
        .await
        .unwrap();
    let question = manager.ask_question(&workspace.name, ask).await.unwrap();
    assert_eq!(reason().await, CleanupReason::Held);
    manager
        .release_hold(&workspace.id, "review".into())
        .await
        .unwrap();
    assert_eq!(reason().await, CleanupReason::Question);
    manager
        .answer_question(question.id, "No".into(), None)
        .await
        .unwrap();
    assert_eq!(
        reason().await,
        CleanupReason::Completed { integrated: false }
    );
    manager.undo_done(&workspace.id).await.unwrap();

    manager
        .set_repository_config(&repo.id, Some("[cleanup.auto]\nenabled = false\n".into()))
        .await
        .unwrap();
    let status = manager.workspace_status(&workspace.id).await.unwrap();
    assert_eq!(status.cleanup_reason, CleanupReason::IdleDisabled);
}

#[tokio::test]
async fn cleanup_preserves_work_and_rechecks_activity_before_deleting() {
    use crate::{
        daemon::{ports::PortRequest, resources::ResourceRequest},
        git,
        test_support::{commit, git as git_in, manager, repository},
    };
    let (temp, manager) = manager().await;
    let repository_dir = repository(temp.path(), "repo");
    fs::write(repository_dir.join(".gitignore"), "ignored/\n").unwrap();
    commit(&repository_dir, ".gitignore");
    crate::test_support::configure(
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
        .acquire_resource(&workspace.id, lease("test-lock", None), None)
        .await
        .unwrap();
    assert!(
        snapshot(&manager).await.is_none(),
        "a resource lease must prevent automatic removal"
    );
    manager
        .release_resource(&workspace.id, "test-lock".into(), "default".into())
        .await
        .unwrap();
    let rwlock_config = workspace.path.join(".shoal.toml");
    fs::write(&rwlock_config, "[resources.cache]\nkind='rwlock'\n").unwrap();
    for mode in [
        crate::daemon::resources::LockMode::Read,
        crate::daemon::resources::LockMode::Write,
    ] {
        manager
            .acquire_resource(&workspace.id, lease("cache", Some(mode)), None)
            .await
            .unwrap();
        // Remove config so only the lease can keep this clean/pushed worktree alive.
        fs::remove_file(&rwlock_config).unwrap();
        assert!(snapshot(&manager).await.is_none());
        manager
            .release_resource(&workspace.id, "cache".into(), "default".into())
            .await
            .unwrap();
        fs::write(&rwlock_config, "[resources.cache]\nkind='rwlock'\n").unwrap();
    }
    fs::remove_file(&rwlock_config).unwrap();
    let original = snapshot(&manager).await.unwrap();
    manager
        .acquire_port(
            &workspace.id,
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
    assert!(manager.remove_idle(&workspace.id, original).await.is_err());
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
            .remove_idle(&workspace.id, after_command)
            .await
            .is_err(),
        "ignored file updates must reset the timer"
    );
    let final_snapshot = snapshot(&manager).await.unwrap();
    // The repository's own idle delay applies before the global default.
    manager
        .set_repository_config(&repo.id, Some("[cleanup.auto]\nidle_minutes = 60\n".into()))
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
                &format!("refs/heads/{}", workspace.branch)
            ],
        )
        .await
        .unwrap(),
        ""
    );
}

#[tokio::test]
async fn requested_cleanup_removes_candidates_without_idle_delay() {
    use crate::test_support::{manager, repository};
    let (temp, manager) = manager().await;
    let checkout = repository(temp.path(), "repo");
    let repo = manager
        .register_repository(checkout.to_str().unwrap().into(), None, None)
        .await
        .unwrap();
    // The request applies where idle cleanup is disabled.
    manager
        .set_repository_config(&repo.id, Some("[cleanup.auto]\nenabled = false\n".into()))
        .await
        .unwrap();
    for name in ["broken", "dirty", "held", "idle"] {
        manager
            .create_workspace(&repo.id, name.into(), None, None, None)
            .await
            .unwrap();
    }
    // An inspection failure is reported rather than read as nothing to do.
    let broken = manager.workspace("broken").await.unwrap();
    fs::remove_file(broken.path.join(".git")).unwrap();
    let dirty = manager.workspace("dirty").await.unwrap();
    fs::write(dirty.path.join("work"), "retain me").unwrap();
    let held = manager.workspace("held").await.unwrap();
    manager
        .acquire_hold(&held.id, "keep".into(), None)
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
            .map(|retained| retained.workspace.clone())
            .collect::<Vec<_>>()
    };

    let all = Selection {
        workspaces: Pick::All,
        ..Selection::default()
    };
    let preview = on_request(&manager, true, &all).await.unwrap();
    assert_eq!(preview.removed, ["idle"]);
    assert_eq!(retained(&preview), ["broken"]);
    let kept = preview
        .kept
        .iter()
        .map(|kept| (kept.workspace.as_str(), kept.reason.clone()))
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
        workspaces: Pick::Only(vec!["dirty".into(), "held".into()]),
        ..Selection::default()
    };
    let report = on_request(&manager, false, &named).await.unwrap();
    assert!(report.removed.is_empty());
    assert_eq!(
        report
            .kept
            .iter()
            .map(|kept| kept.workspace.as_str())
            .collect::<Vec<_>>(),
        ["dirty", "held"]
    );
    let unknown = Selection {
        workspaces: Pick::Only(vec!["missing".into()]),
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
    use crate::test_support::{git, manager, repository};
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
            .acquire_hold(&workspace.id, name.into(), None)
            .await
            .unwrap();
    }
    sweep(&manager, &mut timers).await.unwrap();
    assert!(workspace.path.exists());
    assert!(!git(&checkout, &["branch", "--list", "held"]).is_empty());
    assert!(!timers.idle.contains_key(&workspace.id));
    manager
        .release_hold(&workspace.id, "app-one".into())
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
        .release_hold(&workspace.id, "app-two".into())
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
    use crate::test_support::{commit, manager, repository};
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
