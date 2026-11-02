use super::*;
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
        .mark_done(&done.id, true, false, vec![], &Attacher::User)
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
    let error = failed.workspace(&done.id).unwrap();
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
        .acquire_hold(
            &workspace.id,
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
        .mark_done(&workspace.id, false, false, vec![], &Attacher::User)
        .await
        .unwrap();
    let question = manager.ask_question(&workspace.name, ask).await.unwrap();
    assert_eq!(reason().await, CleanupReason::Held);
    manager
        .release_hold(
            &workspace.id,
            "review".into(),
            crate::model::HoldPlacer::User,
        )
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
        .write_local_config(&repo.id, Some("[cleanup.auto]\nenabled = false\n".into()))
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
    let broken = manager.workspace("broken").await.unwrap();
    fs::remove_file(broken.path.join(".git")).unwrap();
    let dirty = manager.workspace("dirty").await.unwrap();
    fs::write(dirty.path.join("work"), "retain me").unwrap();
    let held = manager.workspace("held").await.unwrap();
    manager
        .acquire_hold(
            &held.id,
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
            .acquire_hold(
                &workspace.id,
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
            &workspace.id,
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
            &workspace.id,
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

#[tokio::test]
async fn git_state_is_inspected_when_the_idle_timer_starts_and_expires() {
    use crate::test_support::{commit, git, manager, repository};
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
    use crate::test_support::{manager, repository};
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
    use crate::{
        daemon::links::pr::Action,
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
            .mark_done(&done.id, true, false, vec![], &Attacher::User)
            .await
            .unwrap();
        let merged = create("merged").await.unwrap();
        manager
            .set_pr(&merged.id, Action::Acknowledge)
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
    use crate::test_support::{manager, repository};
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
    use crate::test_support::{manager, repository};
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
        .split_workspace(&parent.id, "child".into(), None)
        .await
        .unwrap();
    assert_ne!(parent.path.parent(), child.path.parent());
    manager
        .mark_done(&parent.id, true, false, vec![], &Attacher::User)
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
        .map(|notification| (notification.workspace, notification.message))
        .collect();
    assert_eq!(failures.len(), 1, "{failures:?}");
    assert_eq!(failures[0].0.as_deref(), Some("parent"));
    assert!(
        failures[0]
            .1
            .contains("file access stalled in the directory of child, below it"),
        "{failures:?}"
    );
    assert!(parent.path.exists());
    assert!(child.path.exists());
}

/// A registered repository, a parent at an explicit path in another directory
/// than the repository's workspaces, and a workspace there built on it.
async fn split_across_directories(
    base: bool,
) -> (tempfile::TempDir, Arc<Manager>, Workspace, Workspace) {
    use crate::test_support::{manager, repository};
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
            .split_workspace(&parent.id, "child".into(), None)
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
        .mark_done(&parent.id, true, false, vec![], &Attacher::User)
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
    use crate::{
        daemon::links::pr::Action,
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
        .set_pr(&stacked.id, Action::Acknowledge)
        .await
        .unwrap();

    manager.sweep_prs().await.unwrap();

    assert!(!stacked.path.exists());
}
