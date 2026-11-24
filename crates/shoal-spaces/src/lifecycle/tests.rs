use crate::lifecycle::worktree::removal_options;
use crate::lifecycle::*;
use crate::stand_in::Fixture;
use shoal_core::test_support::{git, install_test_script, repository};

use crate::model::EventCause;
use crate::paths::StateFiles;
use crate::{
    cleanup::{Rule, Snapshot},
    events::EventKind,
    git::{
        self,
        worktree::{BranchRemoval, FileRemoval},
    },
    model::{CleanupReason, Workspace},
    process::identity,
    removal::{InspectionPolicy, NOTHING, UnsavedWork},
    state::WorkspaceState,
    workspace_files::Attacher,
};
use shoal_ids::space_ids::SpaceId;
use shoal_protocol::assignments::Done;
use shoal_protocol::log::{LogEntry, LogFilter};
use shoal_protocol::space_names::SpaceSelector;
use shoal_refs::BranchName;
use std::{fs, path::PathBuf, sync::Arc};

pub(super) async fn fixture() -> (Fixture, Workspace) {
    let manager = Fixture::new().await;
    let root = &manager.root;
    let path = repository(root.path(), "repo");
    let repo = manager
        .register_repository(path.to_str().unwrap().into(), None, None)
        .await
        .unwrap();
    let workspace = manager
        .create_workspace(&repo.id, "policy".into(), None, None, None)
        .await
        .unwrap();
    (manager, workspace)
}

pub(super) async fn fixture_with_removal_hook(script: &str) -> (Fixture, Workspace) {
    fixture_with_hook("hooks.pre_remove", |_| script.to_owned()).await
}

/// The kinds and causes of the removal events the space's log shows by
/// default, oldest first.
pub(super) async fn removal_entries(
    manager: &Fixture,
    space: &SpaceSelector,
) -> Vec<(EventKind, Option<EventCause>)> {
    let entries = manager
        .space_log(space, LogFilter::Entries, 50)
        .await
        .unwrap();
    entries
        .into_iter()
        .filter_map(|entry| match entry {
            LogEntry::Event(event) => Some((event.details.kind, event.details.cause)),
            _ => None,
        })
        .filter(|(kind, _)| matches!(kind, EventKind::RemovalStarted | EventKind::Retained))
        .collect()
}

/// A workspace whose repository runs `script(root)` as the `key` hook.
pub(super) async fn fixture_with_hook(
    key: &str,
    script: impl FnOnce(&std::path::Path) -> String,
) -> (Fixture, Workspace) {
    let manager = Fixture::new().await;
    let root = &manager.root;
    let path = repository(root.path(), "repo");
    let hook = root.path().join("remove-hook.sh");
    let script = script(root.path());
    install_test_script(&hook, format!("#!/bin/sh\nset -eu\n{script}\n"));
    fs::write(
        path.join(".shoal.toml"),
        format!("{key} = '{}'\n", hook.display()),
    )
    .unwrap();
    fs::write(path.join(".gitignore"), "ignored/\n").unwrap();
    git(&path, &["add", ".gitignore"]);
    crate::stand_in::commit(&path, ".shoal.toml");
    let repo = manager
        .register_repository(path.to_str().unwrap().into(), None, None)
        .await
        .unwrap();
    let workspace = manager
        .create_workspace(&repo.id, "policy".into(), None, None, None)
        .await
        .unwrap();
    (manager, workspace)
}

async fn corrupt_completion(manager: &Fixture, workspace: &Workspace) {
    let id = workspace.id.clone();
    manager
        .store
        .run(move |db| {
            db.execute(
                "INSERT INTO space_completion(space_id,record) VALUES (?1,'invalid')",
                [id],
            )?;
            Ok(())
        })
        .await
        .unwrap();
    assert!(
        manager
            .spaces
            .inspect_workspace(&**manager, &SpaceSelector::id(&workspace.id))
            .await
            .is_err()
    );
}

/// Start a process outside any execution that carries `workspace` as its
/// identity, as hooks and processes started with `shoal internal env` do.
fn workspace_process(root: &std::path::Path, workspace: &str) -> tokio::process::Child {
    crate::stand_in::spawn_marked_child(
        crate::stand_in::marked_child()
            .env_clear()
            .env(crate::env::SPACE, workspace)
            .current_dir(root)
            .process_group(0),
    )
}

/// The program `built`, a copy of this test binary, built in the worktree
/// of `workspace`. Built output is ignored, as `target/` is, so the
/// worktree stays clean and no checkpoint keeps the program.
fn build_program(workspace: &crate::model::Workspace) -> std::path::PathBuf {
    let common = git(&workspace.path, &["rev-parse", "--git-common-dir"]);
    let exclude = workspace.path.join(common.trim()).join("info/exclude");
    fs::create_dir_all(exclude.parent().unwrap()).unwrap();
    fs::write(&exclude, "built\n").unwrap();
    let built = workspace.path.join("built");
    crate::stand_in::copy_test_binary(&built);
    built
}

/// A workspace, disposable or not, with its worktree.
async fn workspace_of_kind(disposable: bool) -> (Fixture, Workspace) {
    let manager = Fixture::new().await;
    let root = &manager.root;
    let path = repository(root.path(), "repo");
    let repo = manager
        .register_repository(path.to_str().unwrap().into(), None, None)
        .await
        .unwrap();
    let new = crate::workspace::NewWorkspace {
        options: crate::protocol::CreationOptions {
            disposable,
            ..Default::default()
        },
        ..Default::default()
    };
    let workspace = manager
        .spaces
        .create_workspace_with(&*manager, &repo.id, "scratch".into(), None, new)
        .await
        .unwrap();
    (manager, workspace)
}

/// A workspace, disposable or not, whose worktree Git disowned as a
/// `worktree remove` that failed partway leaves it: the directory stays
/// without the admin directory its `.git` names.
async fn disowned_workspace(disposable: bool) -> (Fixture, Workspace) {
    let (manager, workspace) = workspace_of_kind(disposable).await;
    let _root = &manager.root;
    let admin_dir = git(&workspace.path, &["rev-parse", "--absolute-git-dir"]);
    fs::remove_dir_all(admin_dir.trim_end()).unwrap();
    (manager, workspace)
}

/// Remove what is left of the kept `workspace`'s worktree Git disowned,
/// with an untracked file: Git no longer reads what the worktree holds, so
/// removal reads the files left against its branch's commit, deleting one
/// that differs needs consent, and the checkpoint keeps it.
async fn remove_kept_leftover(manager: &Fixture, workspace: &Workspace) {
    fs::write(workspace.path.join("notes.md"), "notes\n").unwrap();
    let selector = SpaceSelector::id(&workspace.id);
    let check = manager
        .spaces
        .check_manual_removal(&**manager, &selector, true)
        .await
        .unwrap();
    let worktree = check.worktree.as_ref().unwrap();
    assert!(worktree.disowned && worktree.unsaved_files_archived);
    assert_eq!(worktree.changed_files, ["?? notes.md"]);
    (manager.spaces.remove_selected(
        &**manager,
        &selector,
        NOTHING.clone(),
        InspectionPolicy::GitOnly,
    ))
    .await
    .unwrap_err();
    assert!(workspace.path.exists());
    let unsaved = UnsavedWork::confirming(&check);
    (manager
        .spaces
        .remove_selected(&**manager, &selector, unsaved, InspectionPolicy::GitOnly))
    .await
    .unwrap();
    assert!(!workspace.path.exists());
    let removed = manager.spaces.removed_space(&selector).await.unwrap();
    let commit = &removed.workspace().unwrap().checkpoint.commit;
    let repo = manager
        .find_repository(workspace.repository())
        .await
        .unwrap();
    let kept = git(&repo.path, &["show", &format!("{commit}:notes.md")]);
    assert_eq!(kept, "notes\n");
}

/// A workspace as a removal that stopped partway leaves it when it cannot
/// put back what it deleted: its marker in the admin directory, a tracked
/// file deleted, and with `failure` the workspace failed with it. With that
/// removal's own failure, the `.git` link is gone too. Returns the admin
/// directory.
async fn cut_short_removal(
    manager: &Fixture,
    workspace: &Workspace,
    failure: Option<&str>,
) -> PathBuf {
    let link = fs::read_to_string(workspace.path.join(".git")).unwrap();
    let admin_dir = workspace.checkout.git_dir().unwrap().to_owned();
    let marker = serde_json::json!({"link": link, "deleted_before": []});
    fs::write(admin_dir.join("shoal-removal"), marker.to_string()).unwrap();
    fs::remove_file(workspace.path.join("tracked")).unwrap();
    if let Some(error) = failure {
        if error.starts_with(STOPPED_REMOVAL) {
            fs::remove_file(workspace.path.join(".git")).unwrap();
        }
        (manager
            .spaces
            .set_state(&workspace.id, WorkspaceState::Failed, Some(error.into())))
        .await
        .unwrap();
    }
    admin_dir
}

const DAEMON_STOPPED: &str = "removal stopped when the daemon stopped";

/// Once something readied the workspace, or failed it otherwise, its
/// deletions may be its agent's: the marker goes and they count as unsaved
/// work.
async fn check_deletions_kept(failure: Option<&str>) {
    let (manager, workspace) = fixture().await;
    let admin_dir = cut_short_removal(&manager, &workspace, failure).await;
    manager
        .spaces
        .remove_selected(
            &*manager,
            &SpaceSelector::id(&workspace.id),
            NOTHING.clone(),
            InspectionPolicy::GitOnly,
        )
        .await
        .unwrap_err();
    assert!(!workspace.path.join("tracked").exists());
    assert!(!admin_dir.join("shoal-removal").exists());
}

/// A disposable workspace as a `worktree add` whose checkout failed leaves
/// it when Git cannot delete every file: `failed` with its pending worktree,
/// without the admin directory its `.git` names, which it never recorded.
async fn failed_checkout() -> (Fixture, Workspace) {
    let (manager, workspace) = disowned_workspace(true).await;
    let _root = &manager.root;
    let id = workspace.id.clone();
    manager
        .store
        .run(move |db| {
            db.execute(
                r#"UPDATE workspaces SET state=?2,git_dir=NULL,
                    pending_plan='{"source":{"kind":"new"}}' WHERE id=?1"#,
                rusqlite::params![id, WorkspaceState::Failed],
            )?;
            Ok(())
        })
        .await
        .unwrap();
    (manager, workspace)
}

/// Removal of what a failed checkout left that it cannot prove is the
/// workspace's: it stays, and the error names it for the user to delete.
async fn retains_unproven(manager: &Fixture, workspace: &Workspace) {
    let error = manager
        .spaces
        .remove_selected(
            &**manager,
            &SpaceSelector::id(&workspace.id),
            UnsavedWork::All,
            InspectionPolicy::GitOnly,
        )
        .await
        .unwrap_err();
    // The error names the directory for the user to delete.
    let path = workspace.path.to_str().unwrap();
    assert!(format!("{error:#}").contains(path), "{error:#}");
    assert!(workspace.path.exists());
    assert!(
        manager
            .spaces
            .find_workspace(&workspace.id)
            .await
            .unwrap()
            .is_some()
    );
}
#[tokio::test]
async fn removal_check_counts_only_its_executions_without_inspection_details() {
    for missing in [false, true] {
        let (manager, workspace) = fixture().await;
        let other = manager
            .create_workspace(workspace.repository(), "other".into(), None, None, None)
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
                    "INSERT INTO executions(id,space_id,state) VALUES ('other',?1,'running')",
                    [other_id],
                )?;
                Ok(())
            })
            .await
            .unwrap();
        let check = manager
            .spaces
            .check_removal(
                &*manager,
                &SpaceSelector::id(&workspace.id),
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
                    "INSERT INTO executions(id,space_id,state,wrapper) VALUES
                     ('running',?1,'running','invalid'), ('unknown',?1,'unknown',NULL)",
                    [id],
                )?;
                Ok(())
            })
            .await
            .unwrap();
        let check = manager
            .spaces
            .check_removal(
                &*manager,
                &SpaceSelector::id(&workspace.id),
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
        let (manager, workspace) = fixture().await;
        corrupt_completion(&manager, &workspace).await;
        if has_executions {
            let id = workspace.id.clone();
            manager
                .store
                .run(move |db| {
                    db.execute(
                        "INSERT INTO executions(id,space_id,state) VALUES ('unknown',?1,'unknown')",
                        [id],
                    )?;
                    Ok(())
                })
                .await
                .unwrap();
        }
        fs::remove_dir_all(&workspace.path).unwrap();
        let result = manager
            .spaces
            .remove_deleted(&*manager, &workspace.id)
            .await;
        if has_executions {
            assert!(result.is_err());
            assert!(
                manager
                    .spaces
                    .workspace_has_executions(&workspace.id)
                    .await
                    .unwrap()
            );
        } else {
            let result = result.unwrap().worktree.unwrap();
            assert!(result.removed);
            assert!(!result.branch_outcome.is_deleted());
        }
        assert_eq!(
            manager.spaces.workspace_by_id(&workspace.id).await.is_ok(),
            has_executions
        );
    }
}

#[tokio::test]
async fn lifecycle_reservation_returns_replaced_state() {
    let (manager, workspace) = fixture().await;
    manager
        .spaces
        .set_state(&workspace.id, WorkspaceState::Failed, None)
        .await
        .unwrap();

    let previous = manager
        .spaces
        .reserve_lifecycle(&workspace.id, WorkspaceState::Removing)
        .await
        .unwrap();

    assert_eq!(previous, WorkspaceState::Failed);
    assert_eq!(
        manager
            .spaces
            .workspace_by_id(&workspace.id)
            .await
            .unwrap()
            .state,
        WorkspaceState::Removing
    );
}

/// A batch checks each target as reserving it would, without reserving, so
/// it can refuse a workspace whose resource hook runs before acting on any.
#[tokio::test]
async fn lifecycle_check_refuses_what_reserving_would_and_reserves_nothing() {
    let (manager, workspace) = fixture().await;
    let hook = manager
        .gates
        .resource_guard(&workspace.id, crate::gates::GuardMode::Exclusive)
        .unwrap();

    assert!(manager.spaces.check_lifecycle(&workspace.id).await.is_err());
    drop(hook);
    manager.spaces.check_lifecycle(&workspace.id).await.unwrap();
    assert_eq!(
        manager
            .spaces
            .workspace_by_id(&workspace.id)
            .await
            .unwrap()
            .state,
        WorkspaceState::Ready
    );
    manager
        .spaces
        .reserve_lifecycle(&workspace.id, WorkspaceState::Stopping)
        .await
        .unwrap();
    assert!(manager.spaces.check_lifecycle(&workspace.id).await.is_err());
}

#[tokio::test]
async fn the_users_removal_takes_a_workspace_whose_setup_never_started() {
    let (manager, workspace) = fixture().await;
    manager
        .spaces
        .set_state(&workspace.id, WorkspaceState::Preparing, None)
        .await
        .unwrap();
    let id = workspace.id.clone();
    manager
        .store
        .run(move |db| {
            db.execute(
                "INSERT INTO executions(id,space_id,state) VALUES ('setup',?1,'running')",
                [id],
            )?;
            Ok(())
        })
        .await
        .unwrap();
    let selected = SpaceSelector::id(&workspace.id);
    let remove = || {
        manager.spaces.remove_selected(
            &*manager,
            &selected,
            UnsavedWork::default(),
            InspectionPolicy::GitOnly,
        )
    };
    assert!(remove().await.is_err());
    assert_eq!(
        manager
            .spaces
            .workspace_by_id(&workspace.id)
            .await
            .unwrap()
            .state,
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
    assert!(manager.spaces.workspace_by_id(&workspace.id).await.is_err());
}

#[tokio::test]
async fn removal_waiting_for_creation_keeps_what_creation_recorded() {
    let (manager, workspace) = fixture().await;
    let base = workspace.checkout.base().cloned();
    assert!(base.as_ref().is_some_and(|base| base.reference.is_some()));
    // Creation holds the Git gate and has yet to record the base.
    let creating = manager.gates.lock_git(workspace.repository()).await;
    let gate = manager.gates.git_gate(workspace.repository());
    let id = workspace.id.clone();
    manager
        .store
        .run(move |db| {
            db.execute(
                r#"UPDATE workspaces SET state='preparing',base_ref=NULL,base_commit=NULL,
                    pending_plan='{"source":{"kind":"new"}}' WHERE id=?1"#,
                [id],
            )?;
            Ok(())
        })
        .await
        .unwrap();
    let removal = tokio::spawn({
        let manager = manager.daemon.clone();
        let id = workspace.id.clone();
        async move {
            manager
                .spaces
                .remove_selected(
                    &*manager,
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
    let (id, recorded) = (workspace.id.clone(), base.clone().unwrap());
    manager
        .store
        .run(move |db| {
            db.execute(
                "UPDATE workspaces SET base_ref=?2,base_commit=?3 WHERE id=?1",
                rusqlite::params![id, recorded.reference, recorded.commit],
            )?;
            Ok(())
        })
        .await
        .unwrap();
    drop(creating);

    assert!(removal.await.unwrap().unwrap().removed);
    let removed = manager.spaces.removed_workspaces().await.unwrap();
    let removed = removed
        .iter()
        .find(|removed| removed.space_id == workspace.id)
        .unwrap();
    assert_eq!(removed.kept_workspace().unwrap().base, base);
}

#[tokio::test]
async fn idle_removal_rechecks_head_and_ignored_activity_after_hooks() {
    for script in [
        "git commit --allow-empty -m 'hook work'",
        "git reset --hard HEAD^",
        "mkdir -p ignored; echo activity > ignored/new",
    ] {
        let (manager, workspace) = fixture_with_removal_hook(script).await;
        let snapshot = manager
            .spaces
            .cleanup_snapshot(&*manager, &workspace.id)
            .await
            .unwrap()
            .unwrap();
        assert!(
            manager
                .spaces
                .remove_idle(&*manager, &SpaceSelector::id(&workspace.id), snapshot)
                .await
                .is_err()
        );
        assert!(workspace.path.exists());
        assert!(manager.spaces.workspace_by_id(&workspace.id).await.is_ok());
        assert!(!git(&workspace.path, &["rev-parse", "refs/heads/policy"]).is_empty());
    }
}

#[tokio::test]
async fn unattended_removal_refuses_retention_its_sweep_missed() {
    let (manager, workspace) = fixture().await;
    let snapshot = manager
        .spaces
        .cleanup_snapshot(&*manager, &workspace.id)
        .await
        .unwrap()
        .unwrap();
    manager
        .spaces
        .acquire_hold(
            &*manager,
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
            .spaces
            .remove(&*manager, &SpaceSelector::id(&workspace.id), removal)
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
        for_repository: false,
        moved: None,
        lives_on: false,
    };
    manager
        .spaces
        .remove(&*manager, &SpaceSelector::id(&workspace.id), removal)
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
        let (manager, workspace) = fixture_with_removal_hook(script).await;
        let removal = match unsaved {
            None => Removal::Automatic {
                snapshot: manager
                    .spaces
                    .cleanup_snapshot(&*manager, &workspace.id)
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
                for_repository: false,
                moved: None,
                lives_on: false,
            },
        };
        assert!(
            manager
                .spaces
                .remove(&*manager, &SpaceSelector::id(&workspace.id), removal)
                .await
                .is_err()
        );
        assert!(workspace.path.exists());
        assert!(manager.spaces.workspace_by_id(&workspace.id).await.is_ok());
    }
}

#[tokio::test]
async fn removal_retains_session_logs_and_places_until_ownership_is_released() {
    let (manager, workspace) = fixture().await;
    let state = manager.paths.space_state(&workspace.id);
    fs::create_dir_all(&state).unwrap();
    let log = state.join("session.log");
    fs::write(&log, "session output").unwrap();
    fs::write(workspace.path.join("untracked"), "work to keep").unwrap();

    assert!(
        manager
            .spaces
            .remove_selected(
                &*manager,
                &SpaceSelector::id(&workspace.id),
                UnsavedWork::default(),
                InspectionPolicy::GitOnly
            )
            .await
            .is_err()
    );
    assert!(manager.spaces.workspace_by_id(&workspace.id).await.is_ok());
    assert_eq!(fs::read_to_string(&log).unwrap(), "session output");
    assert!(manager.closed_places.lock().unwrap().is_empty());

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
    assert!(manager.spaces.workspace_by_id(&workspace.id).await.is_err());
    assert!(!state.exists());
    assert_eq!(*manager.closed_places.lock().unwrap(), [workspace.id]);
}

#[tokio::test]
async fn removal_needs_consent_to_delete_commits_on_a_detached_head() {
    let (manager, workspace) = fixture().await;
    git(&workspace.path, &["switch", "--detach"]);
    fs::write(workspace.path.join("detached-work"), "work to keep\n").unwrap();
    crate::stand_in::commit(&workspace.path, "detached-work");
    let head = git(&workspace.path, &["rev-parse", "HEAD"]);

    let removed = manager
        .spaces
        .remove_selected(
            &*manager,
            &SpaceSelector::id(&workspace.id),
            UnsavedWork::default(),
            InspectionPolicy::GitOnly,
        )
        .await;
    assert!(removed.is_err());
    assert!(workspace.path.is_dir());
    assert!(manager.spaces.workspace_by_id(&workspace.id).await.is_ok());
    assert_eq!(git(&workspace.path, &["rev-parse", "HEAD"]), head);
    assert_eq!(
        fs::read_to_string(workspace.path.join("detached-work")).unwrap(),
        "work to keep\n"
    );

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
    assert!(!workspace.path.exists());
    assert!(manager.spaces.workspace_by_id(&workspace.id).await.is_err());
}

#[tokio::test]
async fn consent_covers_only_the_unsaved_work_it_saw() {
    let (manager, workspace) = fixture().await;
    fs::write(workspace.path.join("draft"), "draft\n").unwrap();
    let check = manager
        .spaces
        .check_manual_removal(&*manager, &SpaceSelector::id(&workspace.id), false)
        .await
        .unwrap();
    let files_only = UnsavedWork::confirming(&check);
    // Commits on a detached HEAD appear after the user confirmed.
    git(&workspace.path, &["switch", "--detach"]);
    fs::write(workspace.path.join("later"), "later\n").unwrap();
    crate::stand_in::commit(&workspace.path, "later");

    let removed = manager
        .spaces
        .remove_selected(
            &*manager,
            &SpaceSelector::id(&workspace.id),
            files_only,
            InspectionPolicy::GitOnly,
        )
        .await;
    assert!(removed.is_err());
    assert!(workspace.path.join("draft").exists());

    let check = manager
        .spaces
        .check_manual_removal(&*manager, &SpaceSelector::id(&workspace.id), false)
        .await
        .unwrap();
    manager
        .spaces
        .remove_selected(
            &*manager,
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
    let (manager, workspace) = fixture().await;
    fs::write(workspace.path.join("draft"), "draft\n").unwrap();
    let promised = |files_restorable| UnsavedWork::Confirmed {
        files: true,
        detached_head: None,
        files_restorable,
        children: None,
    };
    // A repository, which no checkpoint can keep, appeared after the user
    // confirmed.
    repository(&workspace.path, "nested");

    let removed = manager
        .spaces
        .remove_selected(
            &*manager,
            &SpaceSelector::id(&workspace.id),
            promised(true),
            InspectionPolicy::GitOnly,
        )
        .await;
    assert!(removed.is_err());
    assert!(workspace.path.join("draft").exists());

    manager
        .spaces
        .remove_selected(
            &*manager,
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
    let (manager, workspace) = fixture().await;
    let repo = manager
        .find_repository(workspace.repository())
        .await
        .unwrap();
    let head = git(&workspace.path, &["rev-parse", "HEAD"]);
    git(&workspace.path, &["switch", "--detach"]);

    manager
        .spaces
        .remove_selected(
            &*manager,
            &SpaceSelector::id(&workspace.id),
            UnsavedWork::default(),
            InspectionPolicy::GitOnly,
        )
        .await
        .unwrap();
    assert!(!workspace.path.exists());
    assert!(manager.spaces.workspace_by_id(&workspace.id).await.is_err());
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
                for_repository: false,
                moved: None,
                lives_on: false,
            },
            false,
        ),
        (
            Removal::Manual {
                unsaved: &NOTHING,
                inspection: InspectionPolicy::IncludeDirectoryProcesses,
                release_holds: true,
                taking: None,
                for_repository: false,
                moved: None,
                lives_on: false,
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
        let (manager, workspace) = fixture().await;
        let mut child = tokio::process::Command::new("sleep")
            .arg("60")
            .env_clear()
            .env("SHOAL_TEST_PROCESS", "1")
            .current_dir(&workspace.path)
            .kill_on_drop(true)
            .spawn()
            .unwrap();
        let check = manager
            .spaces
            .check_removal(
                &*manager,
                &SpaceSelector::id(&workspace.id),
                removal.inspection(),
            )
            .await
            .unwrap();
        assert_eq!(!check.processes.is_empty(), scans_directory);
        assert!(
            manager
                .spaces
                .cleanup_snapshot(&*manager, &workspace.id)
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
            .spaces
            .remove(&*manager, &SpaceSelector::id(&workspace.id), removal)
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
async fn removal_stops_processes_carrying_the_workspace_identity() {
    let (manager, workspace) = fixture().await;
    let root = &manager.root;
    let mut owned = workspace_process(root.path(), workspace.id.as_str());
    let mut unrelated = workspace_process(root.path(), "another-workspace");
    let owned_identity = identity::capture(owned.id().unwrap()).unwrap().unwrap();
    let unrelated_identity = identity::capture(unrelated.id().unwrap()).unwrap().unwrap();

    manager
        .spaces
        .remove_selected(
            &*manager,
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
    let (manager, workspace) = fixture_with_hook("hooks.post_remove", |root| {
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
    let root = &manager.root;

    let outcome = manager
        .spaces
        .remove_selected(
            &*manager,
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
    let (manager, unpushed) = fixture().await;
    let root = &manager.root;
    let repo = manager
        .find_repository(unpushed.repository())
        .await
        .unwrap();
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
        crate::stand_in::commit(&workspace.path, "work");
    }
    // Pushed without an upstream and never merged.
    git(&pushed.path, &["push", "-q", "origin", "pushed"]);

    for (workspace, deleted) in [(&pushed, true), (&unpushed, false)] {
        let removed = manager
            .spaces
            .remove_selected(
                &*manager,
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
    let (manager, workspace) = fixture().await;
    let repo = manager
        .find_repository(workspace.repository())
        .await
        .unwrap();
    for (inspected_default, later_default, delete) in [
        (
            "main",
            workspace.branch().map(BranchName::as_str).unwrap(),
            true,
        ),
        (
            workspace.branch().map(BranchName::as_str).unwrap(),
            "main",
            false,
        ),
    ] {
        git(
            &repo.path,
            &[
                "symbolic-ref",
                "HEAD",
                &git::local_ref(&inspected_default.into()),
            ],
        );
        let check = manager
            .spaces
            .check_removal(
                &*manager,
                &SpaceSelector::id(&workspace.id),
                InspectionPolicy::GitOnly,
            )
            .await
            .unwrap();
        assert_eq!(check.default_branch.as_deref(), Some(inspected_default));
        assert!(check.branch_preserved());
        git(
            &repo.path,
            &[
                "symbolic-ref",
                "HEAD",
                &git::local_ref(&later_default.into()),
            ],
        );

        let (_, branch) = removal_options(&check, Removal::Deleted, false);
        assert_eq!(matches!(branch, BranchRemoval::Delete), delete);
    }
}

#[tokio::test]
async fn automatic_removal_keeps_branch_when_default_branch_is_unknown() {
    let manager = Fixture::new().await;
    let root = &manager.root;
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
        .spaces
        .adopt_workspace(&*manager, &repo.id, &worktree, false)
        .await
        .unwrap();
    let removed = manager
        .spaces
        .remove_selected(
            &*manager,
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
    let (manager, workspace) = fixture().await;
    let root = &manager.root;
    let detached =
        crate::stand_in::Detached::start(&workspace.path, &root.path().join("pids")).await;
    let mut attached = crate::stand_in::isolated_command("sleep", &workspace.path)
        .arg("300")
        .spawn()
        .unwrap();
    let attached_identity = identity::capture(attached.id().unwrap()).unwrap().unwrap();

    manager
        .spaces
        .remove_selected(
            &*manager,
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
    let (manager, workspace) = fixture().await;
    let root = &manager.root;
    let built = build_program(&workspace);
    let elsewhere = root.path().join("elsewhere");
    fs::create_dir(&elsewhere).unwrap();
    let detached = crate::stand_in::Adopted::start(
        &elsewhere,
        &crate::stand_in::runs_until_stopped(&built),
        &[],
        &root.path().join("pid"),
    )
    .await;

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

    assert!(!identity::alive(&detached.identity).unwrap());
}

/// Automatic cleanup retains a workspace a process works in, but a program
/// built in the worktree and left running elsewhere is no use of it.
#[tokio::test]
async fn automatic_removal_stops_detached_programs_built_in_the_worktree() {
    let (manager, workspace) = fixture().await;
    let root = &manager.root;
    let built = build_program(&workspace);
    let elsewhere = root.path().join("elsewhere");
    fs::create_dir(&elsewhere).unwrap();
    let detached = crate::stand_in::Adopted::start(
        &elsewhere,
        &crate::stand_in::runs_until_stopped(&built),
        &[],
        &root.path().join("pid"),
    )
    .await;
    let snapshot = manager
        .spaces
        .cleanup_snapshot(&*manager, &workspace.id)
        .await
        .unwrap()
        .unwrap();

    manager
        .spaces
        .remove_idle(&*manager, &SpaceSelector::id(&workspace.id), snapshot)
        .await
        .unwrap();

    assert!(!workspace.path.exists());
    assert!(!identity::alive(&detached.identity).unwrap());
}

#[tokio::test]
async fn removal_leaves_detached_processes_in_a_replaced_worktree_alone() {
    // The hook replaces the worktree and leaves a detached process running in
    // the replacement, without the workspace identity hooks pass on.
    let (manager, workspace) = fixture_with_hook("hooks.pre_remove", |root| {
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
    let root = &manager.root;

    let removed = manager
        .spaces
        .remove_selected(
            &*manager,
            &SpaceSelector::id(&workspace.id),
            UnsavedWork::default(),
            InspectionPolicy::GitOnly,
        )
        .await;

    let detached = crate::stand_in::Detached::recorded(&root.path().join("pids")).await;
    assert!(removed.is_err());
    assert!(identity::alive(&detached.shell).unwrap());
    assert!(identity::alive(&detached.child).unwrap());
}

#[tokio::test]
async fn a_removal_moves_results_up_in_the_transaction_that_deletes_it() {
    let manager = Fixture::new().await;
    let temp = &manager.root;
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
        .spaces
        .create_child(
            &*manager,
            &SpaceSelector::id(&top.id),
            "middle".into(),
            None,
            Default::default(),
        )
        .await
        .unwrap();
    let bottom = manager
        .spaces
        .create_child(
            &*manager,
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
        .spaces
        .mark_space_done(
            &*manager,
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
        .spaces
        .remove_selected(
            &*manager,
            &SpaceSelector::id(&bottom.id),
            UnsavedWork::default(),
            InspectionPolicy::GitOnly,
        )
        .await
        .unwrap();
    // The daemon stops right after the middle workspace's removal commits.
    manager
        .spaces
        .delete_space_record(
            &*manager,
            &crate::model::Space::from(middle.clone()),
            EventCause::Manual,
        )
        .await
        .unwrap();
    let id = bottom.id.clone();
    let parent: Option<SpaceId> = manager
        .store
        .run(move |db| {
            Ok(db.query_row(
                "SELECT parent_id FROM space_files WHERE space_id=?1",
                [id],
                |row| row.get(0),
            )?)
        })
        .await
        .unwrap();
    assert_eq!(parent.as_ref(), Some(&top.id));
}

#[tokio::test]
async fn a_clean_confirmation_naming_children_removes_clean_only() {
    let (manager, workspace) = fixture().await;
    let child = manager
        .spaces
        .create_child(
            &*manager,
            &SpaceSelector::id(&workspace.id),
            "child".into(),
            None,
            Default::default(),
        )
        .await
        .unwrap();
    let check = manager
        .spaces
        .check_manual_removal(&*manager, &SpaceSelector::id(&workspace.id), false)
        .await
        .unwrap();
    let confirmed = UnsavedWork::confirming(&check);
    assert_eq!(confirmed.children(), Some(&[child.id][..]));
    // Files appearing after the final inspection are not the caller's to
    // delete, so only unsaved work it confirmed forces removal.
    let check = crate::removal::Inspection {
        check: check.worktree.unwrap(),
        default_branch: None,
        idle_restores: vec![],
    };
    let manual = |unsaved| Removal::Manual {
        unsaved,
        inspection: InspectionPolicy::GitOnly,
        release_holds: true,
        taking: None,
        for_repository: false,
        moved: None,
        lives_on: false,
    };
    let (files, _) = removal_options(&check, manual(&confirmed), false);
    assert!(matches!(files, FileRemoval::CleanOnly));
    let (files, _) = removal_options(&check, manual(&UnsavedWork::All), false);
    assert!(matches!(files, FileRemoval::Force));
}

#[tokio::test]
async fn idle_cleanup_takes_a_disposable_workspace_whatever_it_holds() {
    let manager = Fixture::new().await;
    let root = &manager.root;
    let path = repository(root.path(), "repo");
    let repo = manager
        .register_repository(path.to_str().unwrap().into(), None, None)
        .await
        .unwrap();
    let new = |disposable| crate::workspace::NewWorkspace {
        options: crate::protocol::CreationOptions {
            disposable,
            ..Default::default()
        },
        ..Default::default()
    };
    let disposable = manager
        .spaces
        .create_workspace_with(&*manager, &repo.id, "scratch".into(), None, new(true))
        .await
        .unwrap();
    let kept = manager
        .spaces
        .create_workspace_with(&*manager, &repo.id, "kept".into(), None, new(false))
        .await
        .unwrap();
    for workspace in [&disposable, &kept] {
        fs::write(workspace.path.join("draft"), "unsaved").unwrap();
    }
    assert!(matches!(
        manager
            .spaces
            .cleanup_reason(&*manager, &crate::model::Space::from(disposable.clone()))
            .await,
        CleanupReason::Idle { .. }
    ));
    assert!(!matches!(
        manager
            .spaces
            .cleanup_reason(&*manager, &crate::model::Space::from(kept.clone()))
            .await,
        CleanupReason::Idle { .. }
    ));
}

#[tokio::test]
async fn a_disposable_workspace_deletes_only_its_own_new_branch() {
    let manager = Fixture::new().await;
    let root = &manager.root;
    let path = repository(root.path(), "repo");
    git(&path, &["branch", "kept"]);
    let repo = manager
        .register_repository(path.to_str().unwrap().into(), None, None)
        .await
        .unwrap();
    let disposable = || crate::workspace::NewWorkspace {
        options: crate::protocol::CreationOptions {
            disposable: true,
            ..Default::default()
        },
        ..Default::default()
    };
    // Its removal would delete a branch the user had before it.
    let existing = manager
        .spaces
        .open_branch_with(&*manager, &repo.id, "kept", None, disposable())
        .await;
    assert!(existing.is_err());

    // A disposable workspace switched to another branch leaves that one.
    let workspace = manager
        .spaces
        .create_workspace_with(&*manager, &repo.id, "scratch".into(), None, disposable())
        .await
        .unwrap();
    git(&workspace.path, &["switch", "-q", "kept"]);
    let removed = manager
        .spaces
        .remove_selected(
            &*manager,
            &SpaceSelector::id(&workspace.id),
            UnsavedWork::default(),
            InspectionPolicy::GitOnly,
        )
        .await
        .unwrap();
    assert!(!removed.branch_outcome.is_deleted(), "{removed:?}");
    assert!(!git(&path, &["branch", "--list", "kept"]).is_empty());
    // Nor does cleanup delete it later as a branch the workspace kept.
    assert!(
        manager
            .spaces
            .list_kept_branches(&*manager, None)
            .await
            .unwrap()
            .is_empty()
    );
}

#[tokio::test]
async fn the_log_shows_a_removal_that_failed_once_however_often_it_fails() {
    let (manager, workspace) = fixture_with_removal_hook("exit 1").await;
    let selector = SpaceSelector::id(&workspace.id);
    for _ in 0..2 {
        manager
            .spaces
            .remove_selected(
                &*manager,
                &selector,
                NOTHING.clone(),
                InspectionPolicy::GitOnly,
            )
            .await
            .unwrap_err();
    }
    assert_eq!(
        removal_entries(&manager, &selector).await,
        [
            (EventKind::RemovalStarted, Some(EventCause::Manual)),
            (EventKind::Retained, Some(EventCause::Manual)),
        ]
    );
}

/// A disposable workspace whose directory was deleted while Git still lists
/// it has no directory whose identity a removal could record, and goes.
#[tokio::test]
async fn removal_forgets_a_deleted_disposable_worktree_git_still_lists() {
    let (manager, workspace) = workspace_of_kind(true).await;
    fs::remove_dir_all(&workspace.path).unwrap();
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
    assert!(
        manager
            .spaces
            .find_workspace(&workspace.id)
            .await
            .unwrap()
            .is_none()
    );
}

#[tokio::test]
async fn removal_deletes_a_disposable_worktree_git_disowned() {
    let (manager, workspace) = disowned_workspace(true).await;
    let removed = manager
        .spaces
        .remove_selected(
            &*manager,
            &SpaceSelector::id(&workspace.id),
            UnsavedWork::All,
            InspectionPolicy::GitOnly,
        )
        .await
        .unwrap();
    assert!(!workspace.path.exists());
    assert!(
        manager
            .spaces
            .find_workspace(&workspace.id)
            .await
            .unwrap()
            .is_none()
    );
    // Git can no longer tell whether the worktree was still on it.
    assert!(!removed.branch_outcome.is_deleted(), "{removed:?}");
}

#[tokio::test]
async fn removal_checkpoints_what_is_left_of_a_kept_worktree_git_disowned() {
    let (manager, workspace) = disowned_workspace(false).await;
    remove_kept_leftover(&manager, &workspace).await;
}

/// Without its `.git` link, the directory a removal of it began deleting
/// proves what is left the kept workspace's, as it does a disposable one's.
#[tokio::test]
async fn removal_checkpoints_what_is_left_of_a_kept_worktree_without_its_link() {
    let (manager, workspace) = disowned_workspace(false).await;
    manager
        .spaces
        .record_removal_directory(&workspace)
        .await
        .unwrap();
    fs::remove_file(workspace.path.join(".git")).unwrap();
    remove_kept_leftover(&manager, &workspace).await;
}

/// The tracked files and `.git` link a cut-short removal deleted are nobody's
/// work: the next removal puts them back, finds the worktree clean and
/// removes it without consent to delete unsaved work.
#[tokio::test]
async fn removal_finishes_one_that_stopped_partway() {
    let (manager, workspace) = fixture().await;
    let admin_dir = cut_short_removal(&manager, &workspace, Some(DAEMON_STOPPED)).await;
    manager
        .spaces
        .remove_selected(
            &*manager,
            &SpaceSelector::id(&workspace.id),
            NOTHING.clone(),
            InspectionPolicy::GitOnly,
        )
        .await
        .unwrap();
    assert!(!workspace.path.exists());
    assert!(!admin_dir.exists());
}

/// A retry that fails before it puts the files back keeps the error that
/// marks the workspace for the removal after it.
#[tokio::test]
async fn a_failed_retry_keeps_the_stopped_removal_pending() {
    let (manager, workspace) = fixture().await;
    let admin_dir = cut_short_removal(&manager, &workspace, Some(DAEMON_STOPPED)).await;
    // Ownership no longer verifies, so the retry fails before restoring.
    fs::write(admin_dir.join("shoal-workspace"), "another\n").unwrap();
    let selector = SpaceSelector::id(&workspace.id);
    (manager.spaces.remove_selected(
        &*manager,
        &selector,
        NOTHING.clone(),
        InspectionPolicy::GitOnly,
    ))
    .await
    .unwrap_err();
    let retained = manager.spaces.workspace_by_id(&workspace.id).await.unwrap();
    assert_eq!(retained.state, WorkspaceState::Failed);
    let error = retained.part.error.unwrap_or_default();
    assert!(error.starts_with(STOPPED_REMOVAL), "{error}");
    assert!(admin_dir.join("shoal-removal").exists());
}

#[tokio::test]
async fn a_readied_workspace_keeps_its_deletions() {
    check_deletions_kept(None).await;
}

#[tokio::test]
async fn a_workspace_failed_otherwise_keeps_its_deletions() {
    check_deletions_kept(Some("setup failed")).await;
}

/// Startup puts back what a removal the daemon's stop cut short deleted,
/// before anything readies the workspace or an agent works there.
#[tokio::test]
async fn startup_restores_what_a_cut_short_removal_deleted() {
    let (manager, workspace) = fixture().await;
    let admin_dir = cut_short_removal(&manager, &workspace, Some(DAEMON_STOPPED)).await;
    manager.spaces.restore_interrupted_removals().await.unwrap();
    assert_eq!(git(&workspace.path, &["status", "--porcelain"]), "");
    assert!(!admin_dir.join("shoal-removal").exists());
}

/// A workspace's life from creation to removal, with a removal hook that runs
/// Git, which [`a_runners_repository_variables_reach_no_git_of_the_tests`]
/// runs as the tests of a runner in another repository.
#[tokio::test]
#[ignore = "helper the runner repository test starts"]
async fn workspace_life_with_git_in_a_hook() {
    let (manager, workspace) = fixture_with_removal_hook("git branch hooked").await;
    manager
        .spaces
        .remove_selected(
            &*manager,
            &SpaceSelector::id(&workspace.id),
            UnsavedWork::default(),
            InspectionPolicy::GitOnly,
        )
        .await
        .unwrap();
}

/// A Git hook or `git rebase --exec` running the tests exports `GIT_DIR` and
/// its relatives, which neither the daemon's Git nor what it starts follows.
#[test]
fn a_runners_repository_variables_reach_no_git_of_the_tests() {
    let root = tempfile::tempdir_in("/tmp").unwrap();
    let runner = repository(root.path(), "runner");
    let git_dir = runner.join(".git");
    let state = || {
        [
            &["for-each-ref"][..],
            &["worktree", "list", "--porcelain"],
            &["ls-files", "--stage"],
            &["config", "--local", "--list"],
        ]
        .map(|args| git(&runner, args))
    };
    let before = state();
    let program = std::env::current_exe().unwrap();
    let output = shoal_core::test_support::unmarked(program.to_str().unwrap())
        .args([
            "--exact",
            "lifecycle::tests::workspace_life_with_git_in_a_hook",
            "--ignored",
        ])
        .env("GIT_DIR", &git_dir)
        .env("GIT_WORK_TREE", &runner)
        .env("GIT_INDEX_FILE", git_dir.join("index"))
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stdout)
    );
    assert!(String::from_utf8_lossy(&output.stdout).contains("1 passed"));
    assert_eq!(state(), before);
}

#[tokio::test]
async fn removal_deletes_what_a_failed_checkout_left() {
    let (manager, workspace) = failed_checkout().await;
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
    assert!(!workspace.path.exists());
    assert!(
        manager
            .spaces
            .find_workspace(&workspace.id)
            .await
            .unwrap()
            .is_none()
    );
}

/// Without its `.git` link nothing proves the directory is the one the
/// checkout made.
#[tokio::test]
async fn removal_retains_a_failed_checkouts_directory_without_its_git_link() {
    let (manager, workspace) = failed_checkout().await;
    fs::remove_file(workspace.path.join(".git")).unwrap();
    retains_unproven(&manager, &workspace).await;
}

/// Only an admin directory of the workspace's own repository is one its
/// checkout made.
#[tokio::test]
async fn removal_retains_a_failed_checkouts_directory_linked_to_another_repository() {
    let (manager, workspace) = failed_checkout().await;
    let root = &manager.root;
    let other = repository(root.path(), "other");
    let admin_dir = other.join(".git/worktrees/scratch");
    fs::write(
        workspace.path.join(".git"),
        format!("gitdir: {}\n", admin_dir.display()),
    )
    .unwrap();
    retains_unproven(&manager, &workspace).await;
}

/// The spaces below on other machines go with the cause of the removal
/// they are part of, which an unattended removal's child keeps across
/// machines, so only a removal someone asked for holds theirs for an
/// offline machine.
#[test]
fn removals_below_elsewhere_keep_the_cause_of_the_removal_they_are_part_of() {
    let cause = |removal: Removal<'_>| removal.below_elsewhere().unwrap().cause;
    let idle_child = Removal::Parent {
        cause: EventCause::Idle,
        refuse_holds: false,
        confirmed: None,
        moved: None,
        lives_on: false,
    };
    assert_eq!(cause(idle_child), EventCause::Idle);
    assert_eq!(cause(idle_child.of_child()), EventCause::Idle);
    let asked = Removal::Manual {
        unsaved: &UnsavedWork::All,
        inspection: InspectionPolicy::GitOnly,
        release_holds: true,
        taking: None,
        for_repository: false,
        moved: None,
        lives_on: false,
    };
    assert_eq!(cause(asked.of_child()), EventCause::Manual);
}

#[test]
fn a_space_that_lives_on_elsewhere_ends_here_only_with_the_spaces_it_takes() {
    let ending = |lives_on| Removal::Manual {
        unsaved: &UnsavedWork::All,
        inspection: InspectionPolicy::GitOnly,
        release_holds: true,
        taking: None,
        for_repository: false,
        moved: None,
        lives_on,
    };
    let (moved, removed) = (ending(true), ending(false));
    assert!(!moved.ends_space());
    assert!(!moved.of_child().ends_space());
    assert!(removed.ends_space());
    assert!(removed.of_child().ends_space());
}
