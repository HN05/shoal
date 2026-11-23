use crate::daemon::lifecycle::*;
use crate::daemon::manager::Manager;

use crate::model::EventCause;
use crate::{
    daemon::{
        cleanup::{Rule, Snapshot},
        events::EventKind,
        test_support::{git, install_test_script, manager, repository},
    },
    model::Workspace,
    process::identity,
    protocol::QueuedLaunches,
    removal::{InspectionPolicy, NOTHING, UnsavedWork},
};
use shoal_protocol::log::{LogEntry, LogFilter};
use shoal_protocol::space_names::SpaceSelector;
use std::{fs, sync::Arc};

pub(super) async fn fixture() -> (tempfile::TempDir, Arc<Manager>, Workspace) {
    let (root, manager) = manager().await;
    let path = repository(root.path(), "repo");
    let repo = manager
        .repositories
        .register(path.to_str().unwrap().into(), None, None)
        .await
        .unwrap();
    let workspace = manager
        .create_workspace(&repo.id, "policy".into(), None, None, None)
        .await
        .unwrap();
    (root, manager, workspace)
}

pub(super) async fn fixture_with_removal_hook(
    script: &str,
) -> (tempfile::TempDir, Arc<Manager>, Workspace) {
    fixture_with_hook("hooks.pre_remove", |_| script.to_owned()).await
}

/// The kinds and causes of the removal events the space's log shows by
/// default, oldest first.
pub(super) async fn removal_entries(
    manager: &Manager,
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
) -> (tempfile::TempDir, Arc<Manager>, Workspace) {
    let (root, manager) = manager().await;
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
    crate::daemon::test_support::commit(&path, ".shoal.toml");
    let repo = manager
        .repositories
        .register(path.to_str().unwrap().into(), None, None)
        .await
        .unwrap();
    let workspace = manager
        .create_workspace(&repo.id, "policy".into(), None, None, None)
        .await
        .unwrap();
    (root, manager, workspace)
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
                for_repository: false,
                moved: None,
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
                        "INSERT INTO executions(id,space_id,state) VALUES ('legacy',?1,'unknown')",
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
                .agents
                .stop_space(
                    &*manager,
                    &SpaceSelector::id(&workspace.id),
                    crate::daemon::executions::StopRequest::manual(),
                )
                .await
                .unwrap_err();
            assert!(
                error
                    .downcast_ref::<crate::daemon::executions::UnprovenStop>()
                    .is_some(),
                "{error:#}"
            );
            let retained = matches!(removal, Removal::Automatic { .. } | Removal::Deleted);
            // Exercise the stop policy even when an earlier lifecycle check
            // would reject this removal. Stopping for removal must leave the
            // record intact until the workspace itself is successfully removed.
            let stopped = manager
                .agents
                .stop_executions(
                    &*manager,
                    &workspace.id,
                    removal.stop_policy(),
                    &crate::daemon::executions::StopRequest::removal(),
                )
                .await;
            assert_eq!(stopped.is_err(), retained);
            assert_eq!(
                manager
                    .spaces
                    .inspect_workspace(&*manager, &SpaceSelector::id(&workspace.id))
                    .await
                    .unwrap()
                    .executions
                    .len(),
                1
            );
            let result = manager
                .spaces
                .remove(&*manager, &SpaceSelector::id(&workspace.id), removal)
                .await;
            assert_eq!(result.is_err(), retained);
            assert_eq!(
                manager.spaces.workspace_by_id(&workspace.id).await.is_ok(),
                retained
            );
            if retained {
                assert_eq!(
                    manager
                        .spaces
                        .inspect_workspace(&*manager, &SpaceSelector::id(&workspace.id))
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
                "INSERT INTO executions(id,space_id,state,wrapper) VALUES ('legacy',?1,'unknown',?2)",
                rusqlite::params![id, dead],
            )?;
            Ok(())
        })
        .await
        .unwrap();
    // Waiting for confirmation is not a failure the workspace records.
    let unproven = manager
        .agents
        .stop_space(
            &*manager,
            &SpaceSelector::id(&workspace.id),
            crate::daemon::executions::StopRequest::manual(),
        )
        .await
        .unwrap_err();
    assert!(
        unproven
            .downcast_ref::<crate::daemon::executions::UnprovenStop>()
            .is_some()
    );
    assert_eq!(
        manager
            .spaces
            .workspace_by_id(&workspace.id)
            .await
            .unwrap()
            .error,
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
        .agents
        .stop_space_manually(
            &*manager,
            &SpaceSelector::id(&workspace.id),
            true,
            QueuedLaunches::Cancel,
        )
        .await;
    assert!(blocked.is_err());
    let inspection = manager
        .spaces
        .inspect_workspace(&*manager, &SpaceSelector::id(&workspace.id))
        .await
        .unwrap();
    assert_eq!(inspection.executions.len(), 1);
    user.kill().await.unwrap();
    user.wait().await.unwrap();

    manager
        .agents
        .stop_space_manually(
            &*manager,
            &SpaceSelector::id(&workspace.id),
            true,
            QueuedLaunches::Cancel,
        )
        .await
        .unwrap();
    let inspection = manager
        .spaces
        .inspect_workspace(&*manager, &SpaceSelector::id(&workspace.id))
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
                "INSERT INTO executions(id,space_id,state,wrapper,child,group_id) VALUES ('stale',?1,'unknown',?2,?2,?3)",
                rusqlite::params![id, stale, pid],
            )?;
            Ok(())
        }).await.unwrap();
        let stopped = manager
            .agents
            .stop_space(
                &*manager,
                &SpaceSelector::id(&workspace.id),
                crate::daemon::executions::StopRequest::manual(),
            )
            .await;
        assert!(stopped.is_err());
        // The user's acknowledgement never covers a live process.
        let acknowledged = manager
            .agents
            .stop_space_manually(
                &*manager,
                &SpaceSelector::id(&workspace.id),
                true,
                QueuedLaunches::Cancel,
            )
            .await;
        assert!(acknowledged.is_err());
        assert!(identity::alive(&live).unwrap());
        assert_eq!(
            manager
                .spaces
                .inspect_workspace(&*manager, &SpaceSelector::id(&workspace.id))
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
                for_repository: false,
                moved: None,
            }
        };
        manager
            .spaces
            .remove(&*manager, &SpaceSelector::id(&workspace.id), removal)
            .await
            .unwrap();
        // Removal may discard the record, but an unverified group is never a target.
        assert!(identity::alive(&live).unwrap());
        child.kill().await.unwrap();
        child.wait().await.unwrap();
    }
}
