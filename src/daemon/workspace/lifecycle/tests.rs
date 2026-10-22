use super::*;
use crate::{
    model::Workspace,
    process::identity,
    test_support::{git, manager, repository},
};
use std::{fs, os::unix::fs::PermissionsExt, sync::Arc};

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
    fixture_with_hook("pre_remove_cmd", |_| script.to_owned()).await
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
    fs::write(&hook, format!("#!/bin/sh\nset -eu\n{script}\n")).unwrap();
    fs::set_permissions(&hook, fs::Permissions::from_mode(0o755)).unwrap();
    fs::write(
        path.join(".shoal.toml"),
        format!("{key} = '{}'\n", hook.display()),
    )
    .unwrap();
    fs::write(path.join(".gitignore"), "ignored/\n").unwrap();
    git(&path, &["add", ".gitignore"]);
    crate::test_support::commit(&path, ".shoal.toml");
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
    assert!(manager.inspect_workspace(&workspace.id).await.is_err());
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
            .check_removal(&workspace.name, InspectionPolicy::GitOnly)
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
            .check_removal(&workspace.name, InspectionPolicy::GitOnly)
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
        let result = manager.remove_deleted(&workspace.id).await;
        if has_executions {
            let error = result.unwrap_err();
            assert!(
                error.to_string().contains("commands are recorded"),
                "{error:#}"
            );
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
            manager.workspace(&workspace.id).await.is_ok(),
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
        manager.workspace(&workspace.id).await.unwrap().state,
        WorkspaceState::Removing
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
        let error = manager
            .remove_idle(&workspace.id, snapshot)
            .await
            .unwrap_err();
        assert!(
            error.to_string().contains("during removal hooks"),
            "{error:#}"
        );
        assert!(workspace.path.exists());
        assert!(manager.workspace(&workspace.id).await.is_ok());
        assert!(!git(&workspace.path, &["rev-parse", "refs/heads/policy"]).is_empty());
    }
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
            },
        };
        let error = manager.remove(&workspace.id, removal).await.unwrap_err();
        assert!(error.to_string().contains("ownership"), "{error:#}");
        assert!(workspace.path.exists());
        assert!(manager.workspace(&workspace.id).await.is_ok());
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
                &workspace.id,
                UnsavedWork::default(),
                InspectionPolicy::GitOnly
            )
            .await
            .is_err()
    );
    assert!(manager.workspace(&workspace.id).await.is_ok());
    assert_eq!(fs::read_to_string(&log).unwrap(), "session output");

    manager
        .remove_workspace(&workspace.id, UnsavedWork::All, InspectionPolicy::GitOnly)
        .await
        .unwrap();
    assert!(manager.workspace(&workspace.id).await.is_err());
    assert!(!state.exists());
}

#[tokio::test]
async fn removal_needs_consent_to_delete_commits_on_a_detached_head() {
    let (_root, manager, workspace) = fixture().await;
    git(&workspace.path, &["switch", "--detach"]);
    fs::write(workspace.path.join("detached-work"), "work to keep\n").unwrap();
    crate::test_support::commit(&workspace.path, "detached-work");
    let head = git(&workspace.path, &["rev-parse", "HEAD"]);

    let error = manager
        .remove_workspace(
            &workspace.id,
            UnsavedWork::default(),
            InspectionPolicy::GitOnly,
        )
        .await
        .unwrap_err();
    assert!(
        error.to_string().contains("1 commit(s) on a detached HEAD"),
        "{error:#}"
    );
    assert!(workspace.path.is_dir());
    assert!(manager.workspace(&workspace.id).await.is_ok());
    assert_eq!(git(&workspace.path, &["rev-parse", "HEAD"]), head);
    assert_eq!(
        fs::read_to_string(workspace.path.join("detached-work")).unwrap(),
        "work to keep\n"
    );

    manager
        .remove_workspace(&workspace.id, UnsavedWork::All, InspectionPolicy::GitOnly)
        .await
        .unwrap();
    assert!(!workspace.path.exists());
    assert!(manager.workspace(&workspace.id).await.is_err());
}

#[tokio::test]
async fn consent_covers_only_the_unsaved_work_it_saw() {
    let (_root, manager, workspace) = fixture().await;
    fs::write(workspace.path.join("draft"), "draft\n").unwrap();
    let check = manager
        .check_removal(&workspace.id, InspectionPolicy::GitOnly)
        .await
        .unwrap();
    let files_only = UnsavedWork::confirming(&check);
    // Commits on a detached HEAD appear after the user confirmed.
    git(&workspace.path, &["switch", "--detach"]);
    fs::write(workspace.path.join("later"), "later\n").unwrap();
    crate::test_support::commit(&workspace.path, "later");

    let error = manager
        .remove_workspace(&workspace.id, files_only, InspectionPolicy::GitOnly)
        .await
        .unwrap_err();
    assert!(
        error.to_string().contains("1 commit(s) on a detached HEAD"),
        "{error:#}"
    );
    assert!(workspace.path.join("draft").exists());

    let check = manager
        .check_removal(&workspace.id, InspectionPolicy::GitOnly)
        .await
        .unwrap();
    manager
        .remove_workspace(
            &workspace.id,
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
    };
    // The files grew past the archive limit after the user confirmed.
    crate::test_support::configure(&manager, "[cleanup.restore]\narchive_max_mb = 0\n").await;

    let error = manager
        .remove_workspace(&workspace.id, promised(true), InspectionPolicy::GitOnly)
        .await
        .unwrap_err();
    assert!(
        error
            .to_string()
            .contains("a checkpoint cannot keep the unsaved files"),
        "{error:#}"
    );
    assert!(workspace.path.join("draft").exists());

    manager
        .remove_workspace(&workspace.id, promised(false), InspectionPolicy::GitOnly)
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
            &workspace.id,
            UnsavedWork::default(),
            InspectionPolicy::GitOnly,
        )
        .await
        .unwrap();
    assert!(!workspace.path.exists());
    assert!(manager.workspace(&workspace.id).await.is_err());
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
            },
            false,
        ),
        (
            Removal::Manual {
                unsaved: &NOTHING,
                inspection: InspectionPolicy::IncludeDirectoryProcesses,
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
            .check_removal(&workspace.id, removal.inspection())
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
        let result = manager.remove(&workspace.id, removal).await;
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
                    &workspace.id,
                    crate::daemon::workspace::StopRequest::manual(),
                )
                .await
                .unwrap_err();
            assert!(
                error.to_string().contains("ownership is incomplete"),
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
                    .inspect_workspace(&workspace.id)
                    .await
                    .unwrap()
                    .executions
                    .len(),
                1
            );
            let result = manager.remove(&workspace.id, removal).await;
            assert_eq!(result.is_err(), retained);
            assert_eq!(manager.workspace(&workspace.id).await.is_ok(), retained);
            if retained {
                assert_eq!(
                    manager
                        .inspect_workspace(&workspace.id)
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
        let error = manager
            .stop_workspace(
                &workspace.id,
                crate::daemon::workspace::StopRequest::manual(),
            )
            .await
            .unwrap_err();
        assert!(
            error.to_string().contains("ownership is incomplete"),
            "{error:#}"
        );
        assert!(identity::alive(&live).unwrap());
        assert_eq!(
            manager
                .inspect_workspace(&workspace.id)
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
            }
        };
        manager.remove(&workspace.id, removal).await.unwrap();
        // Removal may discard the record, but an unverified group is never a target.
        assert!(identity::alive(&live).unwrap());
        child.kill().await.unwrap();
        child.wait().await.unwrap();
    }
}

/// Start a process outside any execution that carries `workspace` as its
/// identity, as hooks and processes started with `shoal env` do.
fn workspace_process(root: &std::path::Path, workspace: &str) -> tokio::process::Child {
    tokio::process::Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "process::identity::tests::marked_child",
            "--ignored",
        ])
        .stdout(std::process::Stdio::null())
        .env_clear()
        .env(crate::env::WORKSPACE_ID, workspace)
        .current_dir(root)
        .process_group(0)
        .kill_on_drop(true)
        .spawn()
        .unwrap()
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
            &workspace.id,
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
    let (root, manager, workspace) = fixture_with_hook("post_remove_cmd", |root| {
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
            &workspace.id,
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
        fs::write(workspace.path.join("work"), &workspace.branch).unwrap();
        crate::test_support::commit(&workspace.path, "work");
    }
    // Pushed without an upstream and never merged.
    git(&pushed.path, &["push", "-q", "origin", "pushed"]);

    for (workspace, deleted) in [(&pushed, true), (&unpushed, false)] {
        let removed = manager
            .remove_workspace(
                &workspace.id,
                UnsavedWork::default(),
                InspectionPolicy::GitOnly,
            )
            .await
            .unwrap();
        assert_eq!(removed.branch_outcome.is_deleted(), deleted);
        let branch = git(&repo.path, &["branch", "--list", &workspace.branch]);
        assert_eq!(branch.trim().is_empty(), deleted, "{}", workspace.branch);
    }
}

#[tokio::test]
async fn branch_removal_uses_the_inspected_default_branch() {
    let (_root, manager, workspace) = fixture().await;
    let repo = manager.repository(&workspace.repository_id).await.unwrap();
    for (inspected_default, later_default, delete) in [
        ("main", workspace.branch.as_str(), true),
        (workspace.branch.as_str(), "main", false),
    ] {
        git(
            &repo.path,
            &["symbolic-ref", "HEAD", &git::local_ref(inspected_default)],
        );
        let check = manager
            .check_removal(&workspace.id, InspectionPolicy::GitOnly)
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
    let workspace = manager.adopt_workspace(&repo.id, &worktree).await.unwrap();
    let removed = manager
        .remove_workspace(
            &workspace.id,
            UnsavedWork::default(),
            InspectionPolicy::GitOnly,
        )
        .await
        .unwrap();
    assert!(removed.removed);
    assert!(!worktree.exists());
    assert!(!git(&path, &["rev-parse", "--verify", "refs/heads/main"]).is_empty());
}
