use super::*;
use crate::client::assignments::Done;
use crate::client::space_ids::SpaceId;
use crate::client::space_names::SpaceSelector;
use crate::{
    daemon::{
        test_support::{git, install_test_script, manager, repository},
        workspace_files::Attacher,
    },
    model::Workspace,
    process::identity,
    protocol::QueuedLaunches,
};
use std::{fs, sync::Arc};

async fn fixture() -> (tempfile::TempDir, Arc<Manager>, Workspace) {
    let (root, manager) = manager().await;
    let path = repository(root.path(), "repo");
    let repo = manager
        .register_repository(path.to_str().unwrap().into(), None, None)
        .await
        .unwrap();
    let workspace = manager
        .create_workspace(&repo.id, "policy".into(), None, None, None)
        .await
        .unwrap();
    (root, manager, workspace)
}

async fn fixture_with_removal_hook(script: &str) -> (tempfile::TempDir, Arc<Manager>, Workspace) {
    fixture_with_hook("hooks.pre_remove", |_| script.to_owned()).await
}

/// A workspace whose repository runs `script(root)` as the `key` hook.
async fn fixture_with_hook(
    key: &str,
    script: impl FnOnce(&std::path::Path) -> String,
) -> (tempfile::TempDir, Arc<Manager>, Workspace) {
    let (root, manager) = manager().await;
    let path = repository(root.path(), "repo");
    let hook = root.path().join("remove-hook.sh");
    let script = script(root.path());
    install_test_script(&hook, &format!("#!/bin/sh\nset -eu\n{script}\n"));
    fs::write(
        path.join(".shoal.toml"),
        format!("{key} = '{}'\n", hook.display()),
    )
    .unwrap();
    fs::write(path.join(".gitignore"), "ignored/\n").unwrap();
    git(&path, &["add", ".gitignore"]);
    crate::daemon::test_support::commit(&path, ".shoal.toml");
    let repo = manager
        .register_repository(path.to_str().unwrap().into(), None, None)
        .await
        .unwrap();
    let workspace = manager
        .create_workspace(&repo.id, "policy".into(), None, None, None)
        .await
        .unwrap();
    (root, manager, workspace)
}

async fn corrupt_completion(manager: &Manager, workspace: &Workspace) {
    let id = workspace.id.clone();
    manager
        .store
        .run(move |db| {
            db.execute(
                "INSERT INTO workspace_completion(workspace_id,record) VALUES (?1,'invalid')",
                [id],
            )?;
            Ok(())
        })
        .await
        .unwrap();
    assert!(
        manager
            .inspect_workspace(&SpaceSelector::id(&workspace.id))
            .await
            .is_err()
    );
}

#[tokio::test]
async fn removal_check_counts_only_its_executions_without_inspection_details() {
    for missing in [false, true] {
        let (_root, manager, workspace) = fixture().await;
        let other = manager
            .create_workspace(&workspace.repository_id, "other".into(), None, None, None)
            .await
            .unwrap();
        corrupt_completion(&manager, &workspace).await;
        if missing {
            fs::remove_dir_all(&workspace.path).unwrap();
        }
        let other_id = other.id;
        manager
            .store
            .run(move |db| {
                db.execute(
                    "INSERT INTO executions(id,workspace_id,state) VALUES ('other',?1,'running')",
                    [other_id],
                )?;
                Ok(())
            })
            .await
            .unwrap();
        let check = manager
            .check_removal(
                &SpaceSelector::from(&workspace.name),
                InspectionPolicy::GitOnly,
            )
            .await
            .unwrap();
        assert_eq!(check.running_commands, 0);
        assert!(check.safe());

        let id = workspace.id.clone();
        manager
            .store
            .run(move |db| {
                db.execute(
                    "INSERT INTO executions(id,workspace_id,state,wrapper) VALUES
                     ('running',?1,'running','invalid'), ('unknown',?1,'unknown',NULL)",
                    [id],
                )?;
                Ok(())
            })
            .await
            .unwrap();
        let check = manager
            .check_removal(
                &SpaceSelector::from(&workspace.name),
                InspectionPolicy::GitOnly,
            )
            .await
            .unwrap();
        assert_eq!(check.running_commands, 2);
        assert!(!check.safe());
    }
}

#[tokio::test]
async fn deleted_removal_checks_executions_without_inspection_details() {
    for has_executions in [false, true] {
        let (_root, manager, workspace) = fixture().await;
        corrupt_completion(&manager, &workspace).await;
        if has_executions {
            let id = workspace.id.clone();
            manager
                .store
                .run(move |db| {
                    db.execute(
                        "INSERT INTO executions(id,workspace_id,state) VALUES ('unknown',?1,'unknown')",
                        [id],
                    )?;
                    Ok(())
                })
                .await
                .unwrap();
        }
        fs::remove_dir_all(&workspace.path).unwrap();
        let result = manager
            .remove_deleted(&SpaceSelector::id(&workspace.id))
            .await;
        if has_executions {
            assert!(result.is_err());
            assert!(
                manager
                    .workspace_has_executions(&workspace.id)
                    .await
                    .unwrap()
            );
        } else {
            let result = result.unwrap();
            assert!(result.removed);
            assert!(!result.branch_outcome.is_deleted());
        }
        assert_eq!(
            manager.workspace_by_id(&workspace.id).await.is_ok(),
            has_executions
        );
    }
}

#[tokio::test]
async fn lifecycle_reservation_returns_replaced_state() {
    let (_root, manager, workspace) = fixture().await;
    manager
        .set_state(&workspace.id, WorkspaceState::Failed, None)
        .await
        .unwrap();

    let previous = manager
        .reserve_lifecycle(&workspace.id, WorkspaceState::Removing)
        .await
        .unwrap();

    assert_eq!(previous, WorkspaceState::Failed);
    assert_eq!(
        manager.workspace_by_id(&workspace.id).await.unwrap().state,
        WorkspaceState::Removing
    );
}

/// A batch checks each target as reserving it would, without reserving, so
/// it can refuse a workspace whose resource hook runs before acting on any.
#[tokio::test]
async fn lifecycle_check_refuses_what_reserving_would_and_reserves_nothing() {
    let (_root, manager, workspace) = fixture().await;
    let hook = manager
        .gates
        .resource_guard(&workspace.id, super::super::GuardMode::Exclusive)
        .unwrap();

    assert!(manager.check_lifecycle(&workspace.id).await.is_err());
    drop(hook);
    manager.check_lifecycle(&workspace.id).await.unwrap();
    assert_eq!(
        manager.workspace_by_id(&workspace.id).await.unwrap().state,
        WorkspaceState::Ready
    );
    manager
        .reserve_lifecycle(&workspace.id, WorkspaceState::Stopping)
        .await
        .unwrap();
    assert!(manager.check_lifecycle(&workspace.id).await.is_err());
}

#[tokio::test]
async fn the_users_removal_takes_a_workspace_whose_setup_never_started() {
    let (_root, manager, workspace) = fixture().await;
    manager
        .set_state(&workspace.id, WorkspaceState::Preparing, None)
        .await
        .unwrap();
    let id = workspace.id.clone();
    manager
        .store
        .run(move |db| {
            db.execute(
                "INSERT INTO executions(id,workspace_id,state) VALUES ('setup',?1,'running')",
                [id],
            )?;
            Ok(())
        })
        .await
        .unwrap();
    let selected = SpaceSelector::id(&workspace.id);
    let remove =
        || manager.remove_workspace(&selected, UnsavedWork::default(), InspectionPolicy::GitOnly);
    assert!(remove().await.is_err());
    assert_eq!(
        manager.workspace_by_id(&workspace.id).await.unwrap().state,
        WorkspaceState::Preparing
    );

    manager
        .store
        .run(|db| {
            db.execute("DELETE FROM executions WHERE id='setup'", [])?;
            Ok(())
        })
        .await
        .unwrap();
    assert!(remove().await.unwrap().removed);
    assert!(!workspace.path.exists());
    assert!(manager.workspace_by_id(&workspace.id).await.is_err());
}

#[tokio::test]
async fn removal_waiting_for_creation_keeps_what_creation_recorded() {
    let (_root, manager, workspace) = fixture().await;
    let base = (workspace.base_ref.clone(), workspace.base_commit.clone());
    assert!(base.0.is_some() && base.1.is_some());
    // Creation holds the Git gate and has yet to record the base.
    let creating = manager.gates.lock_git(&workspace.repository_id).await;
    let gate = manager.gates.git_gate(&workspace.repository_id);
    let id = workspace.id.clone();
    manager
        .store
        .run(move |db| {
            db.execute(
                "UPDATE workspaces SET state='preparing',base_ref=NULL,base_commit=NULL WHERE id=?1",
                [id],
            )?;
            Ok(())
        })
        .await
        .unwrap();
    let removal = tokio::spawn({
        let manager = manager.clone();
        let id = workspace.id.clone();
        async move {
            manager
                .remove_workspace(
                    &SpaceSelector::id(&id),
                    UnsavedWork::default(),
                    InspectionPolicy::GitOnly,
                )
                .await
        }
    });
    // Creation, this test and the waiting removal.
    tokio::time::timeout(std::time::Duration::from_secs(60), async {
        while Arc::strong_count(&gate) < 3 {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("the removal reaches the Git gate");
    let (id, recorded) = (workspace.id.clone(), base.clone());
    manager
        .store
        .run(move |db| {
            db.execute(
                "UPDATE workspaces SET base_ref=?2,base_commit=?3 WHERE id=?1",
                rusqlite::params![id, recorded.0, recorded.1],
            )?;
            Ok(())
        })
        .await
        .unwrap();
    drop(creating);

    assert!(removal.await.unwrap().unwrap().removed);
    let removed = manager.removed_workspaces().await.unwrap();
    let removed = removed
        .iter()
        .find(|removed| removed.workspace_id == workspace.id)
        .unwrap();
    assert_eq!(
        (removed.base_ref.clone(), removed.base_commit.clone()),
        base
    );
}

#[tokio::test]
async fn idle_removal_rechecks_head_and_ignored_activity_after_hooks() {
    for script in [
        "git commit --allow-empty -m 'hook work'",
        "git reset --hard HEAD^",
        "mkdir -p ignored; echo activity > ignored/new",
    ] {
        let (_root, manager, workspace) = fixture_with_removal_hook(script).await;
        let snapshot = manager
            .cleanup_snapshot(&workspace.id)
            .await
            .unwrap()
            .unwrap();
        assert!(
            manager
                .remove_idle(&SpaceSelector::id(&workspace.id), snapshot)
                .await
                .is_err()
        );
        assert!(workspace.path.exists());
        assert!(manager.workspace_by_id(&workspace.id).await.is_ok());
        assert!(!git(&workspace.path, &["rev-parse", "refs/heads/policy"]).is_empty());
    }
}

#[tokio::test]
async fn unattended_removal_refuses_retention_its_sweep_missed() {
    let (_root, manager, workspace) = fixture().await;
    let snapshot = manager
        .cleanup_snapshot(&workspace.id)
        .await
        .unwrap()
        .unwrap();
    manager
        .acquire_hold(
            &SpaceSelector::id(&workspace.id),
            "app".into(),
            None,
            crate::model::HoldPlacer::User,
        )
        .await
        .unwrap();
    let removal = Removal::Automatic {
        snapshot,
        cause: EventCause::Idle,
    };
    assert!(
        manager
            .remove(&SpaceSelector::id(&workspace.id), removal)
            .await
            .is_err()
    );
    assert!(workspace.path.exists());

    let unsaved = UnsavedWork::default();
    let removal = Removal::Manual {
        unsaved: &unsaved,
        inspection: InspectionPolicy::GitOnly,
        release_holds: true,
        taking: None,
    };
    manager
        .remove(&SpaceSelector::id(&workspace.id), removal)
        .await
        .unwrap();
    assert!(!workspace.path.exists());
}

#[tokio::test]
async fn every_removal_mode_reverifies_ownership_after_hooks() {
    let script = r#"common=$(git rev-parse --path-format=absolute --git-common-dir)
branch=$(git symbolic-ref --short HEAD)
cd /
git --git-dir="$common" worktree remove --force "$SHOAL_WORKSPACE_PATH"
git --git-dir="$common" worktree add "$SHOAL_WORKSPACE_PATH" "$branch""#;
    for unsaved in [None, Some(UnsavedWork::default()), Some(UnsavedWork::All)] {
        let (_root, manager, workspace) = fixture_with_removal_hook(script).await;
        let removal = match unsaved {
            None => Removal::Automatic {
                snapshot: manager
                    .cleanup_snapshot(&workspace.id)
                    .await
                    .unwrap()
                    .unwrap(),
                cause: EventCause::Idle,
            },
            Some(ref unsaved) => Removal::Manual {
                unsaved,
                inspection: InspectionPolicy::GitOnly,
                release_holds: true,
                taking: None,
            },
        };
        assert!(
            manager
                .remove(&SpaceSelector::id(&workspace.id), removal)
                .await
                .is_err()
        );
        assert!(workspace.path.exists());
        assert!(manager.workspace_by_id(&workspace.id).await.is_ok());
    }
}

#[tokio::test]
async fn removal_retains_session_logs_until_ownership_is_released() {
    let (_root, manager, workspace) = fixture().await;
    let state = manager.paths.workspace_state(&workspace.id);
    fs::create_dir_all(&state).unwrap();
    let log = state.join("session.log");
    fs::write(&log, "session output").unwrap();
    fs::write(workspace.path.join("untracked"), "work to keep").unwrap();

    assert!(
        manager
            .remove_workspace(
                &SpaceSelector::id(&workspace.id),
                UnsavedWork::default(),
                InspectionPolicy::GitOnly
            )
            .await
            .is_err()
    );
    assert!(manager.workspace_by_id(&workspace.id).await.is_ok());
    assert_eq!(fs::read_to_string(&log).unwrap(), "session output");

    manager
        .remove_workspace(
            &SpaceSelector::id(&workspace.id),
            UnsavedWork::All,
            InspectionPolicy::GitOnly,
        )
        .await
        .unwrap();
    assert!(manager.workspace_by_id(&workspace.id).await.is_err());
    assert!(!state.exists());
}

#[tokio::test]
async fn removal_needs_consent_to_delete_commits_on_a_detached_head() {
    let (_root, manager, workspace) = fixture().await;
    git(&workspace.path, &["switch", "--detach"]);
    fs::write(workspace.path.join("detached-work"), "work to keep\n").unwrap();
    crate::daemon::test_support::commit(&workspace.path, "detached-work");
    let head = git(&workspace.path, &["rev-parse", "HEAD"]);

    let removed = manager
        .remove_workspace(
            &SpaceSelector::id(&workspace.id),
            UnsavedWork::default(),
            InspectionPolicy::GitOnly,
        )
        .await;
    assert!(removed.is_err());
    assert!(workspace.path.is_dir());
    assert!(manager.workspace_by_id(&workspace.id).await.is_ok());
    assert_eq!(git(&workspace.path, &["rev-parse", "HEAD"]), head);
    assert_eq!(
        fs::read_to_string(workspace.path.join("detached-work")).unwrap(),
        "work to keep\n"
    );

    manager
        .remove_workspace(
            &SpaceSelector::id(&workspace.id),
            UnsavedWork::All,
            InspectionPolicy::GitOnly,
        )
        .await
        .unwrap();
    assert!(!workspace.path.exists());
    assert!(manager.workspace_by_id(&workspace.id).await.is_err());
}

#[tokio::test]
async fn consent_covers_only_the_unsaved_work_it_saw() {
    let (_root, manager, workspace) = fixture().await;
    fs::write(workspace.path.join("draft"), "draft\n").unwrap();
    let check = manager
        .check_removal(&SpaceSelector::id(&workspace.id), InspectionPolicy::GitOnly)
        .await
        .unwrap();
    let files_only = UnsavedWork::confirming(&check);
    // Commits on a detached HEAD appear after the user confirmed.
    git(&workspace.path, &["switch", "--detach"]);
    fs::write(workspace.path.join("later"), "later\n").unwrap();
    crate::daemon::test_support::commit(&workspace.path, "later");

    let removed = manager
        .remove_workspace(
            &SpaceSelector::id(&workspace.id),
            files_only,
            InspectionPolicy::GitOnly,
        )
        .await;
    assert!(removed.is_err());
    assert!(workspace.path.join("draft").exists());

    let check = manager
        .check_removal(&SpaceSelector::id(&workspace.id), InspectionPolicy::GitOnly)
        .await
        .unwrap();
    manager
        .remove_workspace(
            &SpaceSelector::id(&workspace.id),
            UnsavedWork::confirming(&check),
            InspectionPolicy::GitOnly,
        )
        .await
        .unwrap();
    assert!(!workspace.path.exists());
}

#[tokio::test]
async fn removal_keeps_files_it_promised_to_restore_when_the_checkpoint_cannot() {
    let (_root, manager, workspace) = fixture().await;
    fs::write(workspace.path.join("draft"), "draft\n").unwrap();
    let promised = |files_restorable| UnsavedWork::Confirmed {
        files: true,
        detached_head: None,
        files_restorable,
        children: None,
    };
    // The files grew past the archive limit after the user confirmed.
    crate::daemon::test_support::configure(&manager, "[cleanup.restore]\narchive_max_mb = 0\n")
        .await;

    let removed = manager
        .remove_workspace(
            &SpaceSelector::id(&workspace.id),
            promised(true),
            InspectionPolicy::GitOnly,
        )
        .await;
    assert!(removed.is_err());
    assert!(workspace.path.join("draft").exists());

    manager
        .remove_workspace(
            &SpaceSelector::id(&workspace.id),
            promised(false),
            InspectionPolicy::GitOnly,
        )
        .await
        .unwrap();
    assert!(!workspace.path.exists());
}

#[tokio::test]
async fn removal_of_a_detached_head_without_unpushed_commits_keeps_the_branch() {
    let (_root, manager, workspace) = fixture().await;
    let repo = manager.repository(&workspace.repository_id).await.unwrap();
    let head = git(&workspace.path, &["rev-parse", "HEAD"]);
    git(&workspace.path, &["switch", "--detach"]);

    manager
        .remove_workspace(
            &SpaceSelector::id(&workspace.id),
            UnsavedWork::default(),
            InspectionPolicy::GitOnly,
        )
        .await
        .unwrap();
    assert!(!workspace.path.exists());
    assert!(manager.workspace_by_id(&workspace.id).await.is_err());
    assert_eq!(
        git(&repo.path, &["rev-parse", "--verify", "refs/heads/policy"]),
        head
    );
}

#[tokio::test]
async fn directory_activity_distinguishes_inspection_from_removal_checks() {
    for (removal, scans_directory) in [
        (
            Removal::Manual {
                unsaved: &NOTHING,
                inspection: InspectionPolicy::GitOnly,
                release_holds: true,
                taking: None,
            },
            false,
        ),
        (
            Removal::Manual {
                unsaved: &NOTHING,
                inspection: InspectionPolicy::IncludeDirectoryProcesses,
                release_holds: true,
                taking: None,
            },
            true,
        ),
        (
            Removal::Automatic {
                snapshot: Snapshot {
                    rule: Rule::Idle,
                    signals: 0,
                    tree: 0,
                },
                cause: EventCause::Idle,
            },
            true,
        ),
        (Removal::Merged { head: "" }, true),
    ] {
        let (_root, manager, workspace) = fixture().await;
        let mut child = tokio::process::Command::new("sleep")
            .arg("60")
            .env_clear()
            .env("SHOAL_TEST_PROCESS", "1")
            .current_dir(&workspace.path)
            .kill_on_drop(true)
            .spawn()
            .unwrap();
        let check = manager
            .check_removal(&SpaceSelector::id(&workspace.id), removal.inspection())
            .await
            .unwrap();
        assert_eq!(!check.processes.is_empty(), scans_directory);
        assert!(
            manager
                .cleanup_snapshot(&workspace.id)
                .await
                .unwrap()
                .is_none()
        );
        let head = git(&workspace.path, &["rev-parse", "HEAD"]);
        let removal = match removal {
            Removal::Merged { .. } => Removal::Merged { head: head.trim() },
            other => other,
        };
        let result = manager
            .remove(&SpaceSelector::id(&workspace.id), removal)
            .await;
        assert_eq!(
            result.is_err(),
            matches!(removal, Removal::Automatic { .. })
        );
        assert!(child.try_wait().unwrap().is_none());
        child.kill().await.unwrap();
        child.wait().await.unwrap();
    }
}

#[tokio::test]
async fn unknown_execution_blocks_stop_and_unattended_removal_even_when_missing() {
    for missing in [false, true] {
        for removal in [
            Removal::Manual {
                unsaved: &UnsavedWork::All,
                inspection: InspectionPolicy::GitOnly,
                release_holds: true,
                taking: None,
            },
            Removal::Automatic {
                snapshot: Snapshot {
                    rule: Rule::Idle,
                    signals: 0,
                    tree: 0,
                },
                cause: EventCause::Idle,
            },
            Removal::Merged { head: "" },
            Removal::Deleted,
        ] {
            let (_root, manager, workspace) = fixture().await;
            let head = git(&workspace.path, &["rev-parse", "HEAD"]);
            let removal = match removal {
                Removal::Merged { .. } => Removal::Merged { head: head.trim() },
                other => other,
            };
            let id = workspace.id.clone();
            manager
                .store
                .run(move |db| {
                    db.execute(
                    "INSERT INTO executions(id,workspace_id,state) VALUES ('legacy',?1,'unknown')",
                    [id],
                )?;
                    Ok(())
                })
                .await
                .unwrap();
            if missing {
                fs::remove_dir_all(&workspace.path).unwrap();
            }
            let error = manager
                .stop_workspace(
                    &SpaceSelector::id(&workspace.id),
                    crate::daemon::workspace::StopRequest::manual(),
                )
                .await
                .unwrap_err();
            assert!(
                error
                    .downcast_ref::<crate::daemon::workspace::UnprovenStop>()
                    .is_some(),
                "{error:#}"
            );
            let retained = matches!(removal, Removal::Automatic { .. } | Removal::Deleted);
            // Exercise the stop policy even when an earlier lifecycle check
            // would reject this removal. Stopping for removal must leave the
            // record intact until the workspace itself is successfully removed.
            let stopped = manager
                .stop_executions(
                    &workspace.id,
                    removal.stop_policy(),
                    &crate::daemon::workspace::StopRequest::removal(),
                )
                .await;
            assert_eq!(stopped.is_err(), retained);
            assert_eq!(
                manager
                    .inspect_workspace(&SpaceSelector::id(&workspace.id))
                    .await
                    .unwrap()
                    .executions
                    .len(),
                1
            );
            let result = manager
                .remove(&SpaceSelector::id(&workspace.id), removal)
                .await;
            assert_eq!(result.is_err(), retained);
            assert_eq!(
                manager.workspace_by_id(&workspace.id).await.is_ok(),
                retained
            );
            if retained {
                assert_eq!(
                    manager
                        .inspect_workspace(&SpaceSelector::id(&workspace.id))
                        .await
                        .unwrap()
                        .executions
                        .len(),
                    1
                );
            } else if missing {
                assert!(!result.unwrap().branch_outcome.is_deleted());
            }
        }
    }
}

#[tokio::test]
async fn acknowledged_stop_forgets_an_execution_without_launch_identity() {
    let (_root, manager, workspace) = fixture().await;
    // A wrapper that died before launching its command recorded no group.
    // Born during the test, it keeps the runner's older processes with
    // unreadable environments out of the evidence.
    let mut wrapper = tokio::process::Command::new("sleep")
        .arg("60")
        .kill_on_drop(true)
        .spawn()
        .unwrap();
    let dead = identity::capture(wrapper.id().unwrap()).unwrap().unwrap();
    wrapper.kill().await.unwrap();
    wrapper.wait().await.unwrap();
    let (id, dead) = (workspace.id.clone(), serde_json::to_string(&dead).unwrap());
    manager
        .store
        .run(move |db| {
            db.execute(
                "INSERT INTO executions(id,workspace_id,state,wrapper) VALUES ('legacy',?1,'unknown',?2)",
                rusqlite::params![id, dead],
            )?;
            Ok(())
        })
        .await
        .unwrap();
    // Waiting for confirmation is not a failure the workspace records.
    let unproven = manager
        .stop_workspace(
            &SpaceSelector::id(&workspace.id),
            crate::daemon::workspace::StopRequest::manual(),
        )
        .await
        .unwrap_err();
    assert!(
        unproven
            .downcast_ref::<crate::daemon::workspace::UnprovenStop>()
            .is_some()
    );
    assert_eq!(
        manager.workspace_by_id(&workspace.id).await.unwrap().error,
        None
    );
    // A process using the directory blocks even an acknowledged stop.
    let mut user = tokio::process::Command::new("sleep")
        .arg("60")
        .current_dir(&workspace.path)
        .kill_on_drop(true)
        .spawn()
        .unwrap();
    let blocked = manager
        .stop_workspace_manually(
            &SpaceSelector::id(&workspace.id),
            true,
            None,
            QueuedLaunches::Cancel,
        )
        .await;
    assert!(blocked.is_err());
    let inspection = manager
        .inspect_workspace(&SpaceSelector::id(&workspace.id))
        .await
        .unwrap();
    assert_eq!(inspection.executions.len(), 1);
    user.kill().await.unwrap();
    user.wait().await.unwrap();

    manager
        .stop_workspace_manually(
            &SpaceSelector::id(&workspace.id),
            true,
            None,
            QueuedLaunches::Cancel,
        )
        .await
        .unwrap();
    let inspection = manager
        .inspect_workspace(&SpaceSelector::id(&workspace.id))
        .await
        .unwrap();
    assert!(inspection.executions.is_empty());
}

#[tokio::test]
async fn stale_birth_identity_never_authorizes_signaling_a_live_group() {
    for merged in [false, true] {
        let (_root, manager, workspace) = fixture().await;
        let mut child = tokio::process::Command::new("sleep")
            .arg("60")
            .env_clear()
            .env("SHOAL_TEST_PROCESS", "1")
            .current_dir(_root.path())
            .process_group(0)
            .kill_on_drop(true)
            .spawn()
            .unwrap();
        let live = identity::capture(child.id().unwrap()).unwrap().unwrap();
        let stale = serde_json::to_string(&identity::Identity {
            birth: "different-process".into(),
            ..live.clone()
        })
        .unwrap();
        let (id, pid) = (workspace.id.clone(), live.pid);
        manager.store.run(move |db| {
            db.execute(
                "INSERT INTO executions(id,workspace_id,state,wrapper,child,group_id) VALUES ('stale',?1,'unknown',?2,?2,?3)",
                rusqlite::params![id, stale, pid],
            )?;
            Ok(())
        }).await.unwrap();
        let stopped = manager
            .stop_workspace(
                &SpaceSelector::id(&workspace.id),
                crate::daemon::workspace::StopRequest::manual(),
            )
            .await;
        assert!(stopped.is_err());
        // The user's acknowledgement never covers a live process.
        let acknowledged = manager
            .stop_workspace_manually(
                &SpaceSelector::id(&workspace.id),
                true,
                None,
                QueuedLaunches::Cancel,
            )
            .await;
        assert!(acknowledged.is_err());
        assert!(identity::alive(&live).unwrap());
        assert_eq!(
            manager
                .inspect_workspace(&SpaceSelector::id(&workspace.id))
                .await
                .unwrap()
                .executions
                .len(),
            1
        );
        let head = git(&workspace.path, &["rev-parse", "HEAD"]);
        let removal = if merged {
            Removal::Merged { head: head.trim() }
        } else {
            Removal::Manual {
                unsaved: &NOTHING,
                inspection: InspectionPolicy::GitOnly,
                release_holds: true,
                taking: None,
            }
        };
        manager
            .remove(&SpaceSelector::id(&workspace.id), removal)
            .await
            .unwrap();
        // Removal may discard the record, but an unverified group is never a target.
        assert!(identity::alive(&live).unwrap());
        child.kill().await.unwrap();
        child.wait().await.unwrap();
    }
}

/// Start a process outside any execution that carries `workspace` as its
/// identity, as hooks and processes started with `shoal work env` do.
fn workspace_process(root: &std::path::Path, workspace: &str) -> tokio::process::Child {
    crate::daemon::test_support::spawn_marked_child(
        crate::daemon::test_support::marked_child()
            .env_clear()
            .env(crate::env::WORKSPACE_ID, workspace)
            .current_dir(root)
            .process_group(0),
    )
}

#[tokio::test]
async fn removal_stops_processes_carrying_the_workspace_identity() {
    let (root, manager, workspace) = fixture().await;
    let mut owned = workspace_process(root.path(), &workspace.id);
    let mut unrelated = workspace_process(root.path(), "another-workspace");
    let owned_identity = identity::capture(owned.id().unwrap()).unwrap().unwrap();
    let unrelated_identity = identity::capture(unrelated.id().unwrap()).unwrap().unwrap();

    manager
        .remove_workspace(
            &SpaceSelector::id(&workspace.id),
            UnsavedWork::default(),
            InspectionPolicy::GitOnly,
        )
        .await
        .unwrap();

    assert!(!identity::alive(&owned_identity).unwrap());
    assert!(identity::alive(&unrelated_identity).unwrap());
    owned.wait().await.unwrap();
    unrelated.kill().await.unwrap();
}

#[tokio::test]
async fn removal_stops_processes_its_post_remove_hook_leaves() {
    // The hook returns once its background process has written its PID.
    let (root, manager, workspace) = fixture_with_hook("hooks.post_remove", |root| {
        let pid = root.join("background");
        format!(
            "python3 -c \"import os, time; os.setsid(); \
             open('{pid}.tmp', 'w').write(str(os.getpid())); \
             os.rename('{pid}.tmp', '{pid}'); time.sleep(300)\" >/dev/null 2>&1 &\n\
             while [ ! -e '{pid}' ]; do sleep 1; done",
            pid = pid.display()
        )
    })
    .await;

    let outcome = manager
        .remove_workspace(
            &SpaceSelector::id(&workspace.id),
            UnsavedWork::default(),
            InspectionPolicy::GitOnly,
        )
        .await
        .unwrap();

    assert_eq!(outcome.hook_error, None);
    let pid = fs::read_to_string(root.path().join("background")).unwrap();
    assert_eq!(identity::capture(pid.parse().unwrap()).unwrap(), None);
}

#[tokio::test]
async fn removal_deletes_a_branch_a_remote_has_and_keeps_unpushed_work() {
    let (root, manager, unpushed) = fixture().await;
    let repo = manager.repository(&unpushed.repository_id).await.unwrap();
    let pushed = manager
        .create_workspace(&repo.id, "pushed".into(), None, None, None)
        .await
        .unwrap();
    let origin = root.path().join("origin.git");
    git(
        root.path(),
        &["init", "-q", "--bare", origin.to_str().unwrap()],
    );
    git(
        &repo.path,
        &["remote", "add", "origin", origin.to_str().unwrap()],
    );
    git(&repo.path, &["push", "-q", "origin", "main"]);
    git(&repo.path, &["remote", "set-head", "origin", "main"]);
    for workspace in [&unpushed, &pushed] {
        fs::write(workspace.path.join("work"), workspace.branch_or_name()).unwrap();
        crate::daemon::test_support::commit(&workspace.path, "work");
    }
    // Pushed without an upstream and never merged.
    git(&pushed.path, &["push", "-q", "origin", "pushed"]);

    for (workspace, deleted) in [(&pushed, true), (&unpushed, false)] {
        let removed = manager
            .remove_workspace(
                &SpaceSelector::id(&workspace.id),
                UnsavedWork::default(),
                InspectionPolicy::GitOnly,
            )
            .await
            .unwrap();
        assert_eq!(removed.branch_outcome.is_deleted(), deleted);
        let branch = git(
            &repo.path,
            &["branch", "--list", workspace.branch_or_name()],
        );
        assert_eq!(
            branch.trim().is_empty(),
            deleted,
            "{}",
            workspace.branch_or_name()
        );
    }
}

#[tokio::test]
async fn branch_removal_uses_the_inspected_default_branch() {
    let (_root, manager, workspace) = fixture().await;
    let repo = manager.repository(&workspace.repository_id).await.unwrap();
    for (inspected_default, later_default, delete) in [
        ("main", workspace.branch.as_deref().unwrap(), true),
        (workspace.branch.as_deref().unwrap(), "main", false),
    ] {
        git(
            &repo.path,
            &["symbolic-ref", "HEAD", &git::local_ref(inspected_default)],
        );
        let check = manager
            .check_removal(&SpaceSelector::id(&workspace.id), InspectionPolicy::GitOnly)
            .await
            .unwrap();
        assert_eq!(check.default_branch.as_deref(), Some(inspected_default));
        assert!(check.branch_preserved());
        git(
            &repo.path,
            &["symbolic-ref", "HEAD", &git::local_ref(later_default)],
        );

        let (_, branch) = worktrunk_removal_options(&check, &NOTHING);
        assert_eq!(matches!(branch, BranchRemoval::Delete), delete);
    }
}

#[tokio::test]
async fn automatic_removal_keeps_branch_when_default_branch_is_unknown() {
    let (root, manager) = manager().await;
    let path = repository(root.path(), "repo");
    let origin = root.path().join("origin.git");
    git(
        &path,
        &[
            "clone",
            "--bare",
            path.to_str().unwrap(),
            origin.to_str().unwrap(),
        ],
    );
    git(
        &path,
        &["remote", "add", "origin", origin.to_str().unwrap()],
    );
    git(&path, &["fetch", "origin"]);
    git(&path, &["branch", "--set-upstream-to=origin/main", "main"]);
    // Model a remote without origin/HEAD, which fetch may have recorded.
    git(&path, &["remote", "set-head", "origin", "-d"]);
    git(&path, &["switch", "-c", "other"]);
    let worktree = root.path().join("main-worktree");
    git(
        &path,
        &["worktree", "add", worktree.to_str().unwrap(), "main"],
    );
    let repo = manager
        .register_repository(path.to_str().unwrap().into(), None, None)
        .await
        .unwrap();
    let workspace = manager
        .adopt_workspace(&repo.id, &worktree, false)
        .await
        .unwrap();
    let removed = manager
        .remove_workspace(
            &SpaceSelector::id(&workspace.id),
            UnsavedWork::default(),
            InspectionPolicy::GitOnly,
        )
        .await
        .unwrap();
    assert!(removed.removed);
    assert!(!worktree.exists());
    assert!(!git(&path, &["rev-parse", "--verify", "refs/heads/main"]).is_empty());
}

#[tokio::test]
async fn removal_stops_detached_processes_running_from_the_worktree() {
    let (root, manager, workspace) = fixture().await;
    let detached =
        crate::daemon::test_support::Detached::start(&workspace.path, &root.path().join("pids"))
            .await;
    let mut attached = crate::daemon::test_support::isolated_command("sleep", &workspace.path)
        .arg("300")
        .spawn()
        .unwrap();
    let attached_identity = identity::capture(attached.id().unwrap()).unwrap().unwrap();

    manager
        .remove_workspace(
            &SpaceSelector::id(&workspace.id),
            UnsavedWork::default(),
            InspectionPolicy::GitOnly,
        )
        .await
        .unwrap();

    assert!(!identity::alive(&detached.shell).unwrap());
    assert!(!identity::alive(&detached.child).unwrap());
    // A process a live parent outside the worktree keeps, such as a shell
    // the user opened there, is not the workspace's to stop.
    assert!(identity::alive(&attached_identity).unwrap());
    attached.kill().await.unwrap();
}

/// A program built in the worktree and left running elsewhere, as a
/// recording's daemon is, is the workspace's to stop.
#[tokio::test]
async fn removal_stops_detached_programs_built_in_the_worktree() {
    let (root, manager, workspace) = fixture().await;
    let built = workspace.path.join("built");
    crate::daemon::test_support::copy_test_binary(&built).await;
    let elsewhere = root.path().join("elsewhere");
    fs::create_dir(&elsewhere).unwrap();
    let detached = crate::daemon::test_support::Adopted::start(
        &elsewhere,
        &crate::daemon::test_support::runs_until_stopped(&built),
        &[],
        &root.path().join("pid"),
    )
    .await;

    manager
        .remove_workspace(
            &SpaceSelector::id(&workspace.id),
            UnsavedWork::All,
            InspectionPolicy::GitOnly,
        )
        .await
        .unwrap();

    assert!(!identity::alive(&detached.identity).unwrap());
}

/// Automatic cleanup retains a workspace a process works in, but a program
/// built in the worktree and left running elsewhere is no use of it.
#[tokio::test]
async fn automatic_removal_stops_detached_programs_built_in_the_worktree() {
    let (root, manager, workspace) = fixture().await;
    // Built output is ignored, as `target/` is, so the worktree stays clean.
    let common = git(&workspace.path, &["rev-parse", "--git-common-dir"]);
    let exclude = workspace.path.join(common.trim()).join("info/exclude");
    fs::create_dir_all(exclude.parent().unwrap()).unwrap();
    fs::write(&exclude, "built\n").unwrap();
    let built = workspace.path.join("built");
    crate::daemon::test_support::copy_test_binary(&built).await;
    let elsewhere = root.path().join("elsewhere");
    fs::create_dir(&elsewhere).unwrap();
    let detached = crate::daemon::test_support::Adopted::start(
        &elsewhere,
        &crate::daemon::test_support::runs_until_stopped(&built),
        &[],
        &root.path().join("pid"),
    )
    .await;
    let snapshot = manager
        .cleanup_snapshot(&workspace.id)
        .await
        .unwrap()
        .unwrap();

    manager
        .remove_idle(&SpaceSelector::id(&workspace.id), snapshot)
        .await
        .unwrap();

    assert!(!workspace.path.exists());
    assert!(!identity::alive(&detached.identity).unwrap());
}

#[tokio::test]
async fn removal_leaves_detached_processes_in_a_replaced_worktree_alone() {
    // The hook replaces the worktree and leaves a detached process running in
    // the replacement, without the workspace identity hooks pass on.
    let (root, manager, workspace) = fixture_with_hook("hooks.pre_remove", |root| {
        format!(
            r#"common=$(git rev-parse --path-format=absolute --git-common-dir)
branch=$(git symbolic-ref --short HEAD)
cd /
git --git-dir="$common" worktree remove --force "$SHOAL_WORKSPACE_PATH"
git --git-dir="$common" worktree add "$SHOAL_WORKSPACE_PATH" "$branch"
cd "$SHOAL_WORKSPACE_PATH"
env -i PATH="$PATH" sh -c 'sleep 300 & echo $$ $! > {pids}.tmp; mv {pids}.tmp {pids}; wait' >/dev/null 2>&1 &
while [ ! -e '{pids}' ]; do sleep 1; done"#,
            pids = root.join("pids").display()
        )
    })
    .await;

    let removed = manager
        .remove_workspace(
            &SpaceSelector::id(&workspace.id),
            UnsavedWork::default(),
            InspectionPolicy::GitOnly,
        )
        .await;

    let detached = crate::daemon::test_support::Detached::recorded(&root.path().join("pids")).await;
    assert!(removed.is_err());
    assert!(identity::alive(&detached.shell).unwrap());
    assert!(identity::alive(&detached.child).unwrap());
}

#[tokio::test]
async fn a_removal_moves_results_up_in_the_transaction_that_deletes_it() {
    let (temp, manager) = manager().await;
    let checkout = repository(temp.path(), "repo");
    let repo = manager
        .register_repository(checkout.to_str().unwrap().into(), None, None)
        .await
        .unwrap();
    let top = manager
        .create_workspace(&repo.id, "top".into(), None, None, None)
        .await
        .unwrap();
    let middle = manager
        .split_workspace(
            &SpaceSelector::id(&top.id),
            "middle".into(),
            None,
            Default::default(),
        )
        .await
        .unwrap();
    let bottom = manager
        .split_workspace(
            &SpaceSelector::id(&middle.id),
            "bottom".into(),
            None,
            Default::default(),
        )
        .await
        .unwrap();
    let report = temp.path().join("report.md");
    fs::write(&report, "No leaks").unwrap();
    manager
        .mark_done(
            &SpaceSelector::id(&bottom.id),
            Done {
                results: vec![report.into()],
                ..Done::default()
            },
            &Attacher::User,
        )
        .await
        .unwrap();
    manager
        .remove_workspace(
            &SpaceSelector::id(&bottom.id),
            UnsavedWork::default(),
            InspectionPolicy::GitOnly,
        )
        .await
        .unwrap();
    // The daemon stops right after the middle workspace's removal commits.
    manager
        .commit_workspace_removal(&middle.id, EventCause::Manual)
        .await
        .unwrap();
    let id = bottom.id.clone();
    let parent: Option<SpaceId> = manager
        .store
        .run(move |db| {
            Ok(db.query_row(
                "SELECT parent_id FROM workspace_files WHERE space_id=?1",
                [id],
                |row| row.get(0),
            )?)
        })
        .await
        .unwrap();
    assert_eq!(parent.as_deref(), Some(top.id.as_str()));
}

#[tokio::test]
async fn a_clean_confirmation_naming_children_removes_clean_only() {
    let (_root, manager, workspace) = fixture().await;
    let child = manager
        .split_workspace(
            &SpaceSelector::id(&workspace.id),
            "child".into(),
            None,
            Default::default(),
        )
        .await
        .unwrap();
    let check = manager
        .check_manual_removal(&SpaceSelector::id(&workspace.id), false)
        .await
        .unwrap();
    let confirmed = UnsavedWork::confirming(&check);
    assert_eq!(confirmed.children(), Some(&[child.id][..]));
    // Files appearing after the final inspection are not the caller's to
    // delete, so only unsaved work it confirmed forces removal.
    let check = crate::removal::Inspection {
        check,
        default_branch: None,
        idle_restores: vec![],
    };
    let (files, _) = worktrunk_removal_options(&check, &confirmed);
    assert!(matches!(files, FileRemoval::CleanOnly));
    let (files, _) = worktrunk_removal_options(&check, &UnsavedWork::All);
    assert!(matches!(files, FileRemoval::Force));
}

#[tokio::test]
async fn idle_cleanup_takes_a_disposable_workspace_whatever_it_holds() {
    let (root, manager) = manager().await;
    let path = repository(root.path(), "repo");
    let repo = manager
        .register_repository(path.to_str().unwrap().into(), None, None)
        .await
        .unwrap();
    let new = |disposable| super::super::NewWorkspace {
        options: crate::protocol::CreationOptions {
            disposable,
            ..Default::default()
        },
        ..Default::default()
    };
    let disposable = manager
        .create_workspace_with(&repo.id, "scratch".into(), None, new(true))
        .await
        .unwrap();
    let kept = manager
        .create_workspace_with(&repo.id, "kept".into(), None, new(false))
        .await
        .unwrap();
    for workspace in [&disposable, &kept] {
        fs::write(workspace.path.join("draft"), "unsaved").unwrap();
    }
    assert!(matches!(
        manager.cleanup_reason(&disposable).await,
        CleanupReason::Idle { .. }
    ));
    assert!(!matches!(
        manager.cleanup_reason(&kept).await,
        CleanupReason::Idle { .. }
    ));
}

#[tokio::test]
async fn a_disposable_workspace_deletes_only_its_own_new_branch() {
    let (root, manager) = manager().await;
    let path = repository(root.path(), "repo");
    git(&path, &["branch", "kept"]);
    let repo = manager
        .register_repository(path.to_str().unwrap().into(), None, None)
        .await
        .unwrap();
    let disposable = || super::super::NewWorkspace {
        options: crate::protocol::CreationOptions {
            disposable: true,
            ..Default::default()
        },
        ..Default::default()
    };
    // Its removal would delete a branch the user had before it.
    let existing = manager
        .open_branch_with(&repo.id, "kept", None, disposable())
        .await;
    assert!(existing.is_err());

    // A disposable workspace switched to another branch leaves that one.
    let workspace = manager
        .create_workspace_with(&repo.id, "scratch".into(), None, disposable())
        .await
        .unwrap();
    git(&workspace.path, &["switch", "-q", "kept"]);
    let removed = manager
        .remove_workspace(
            &SpaceSelector::id(&workspace.id),
            UnsavedWork::default(),
            InspectionPolicy::GitOnly,
        )
        .await
        .unwrap();
    assert!(!removed.branch_outcome.is_deleted(), "{removed:?}");
    assert!(!git(&path, &["branch", "--list", "kept"]).is_empty());
    // Nor does cleanup delete it later as a branch the workspace kept.
    assert!(manager.list_kept_branches(None).await.unwrap().is_empty());
}
