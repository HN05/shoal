use super::*;
use crate::{
    model::Workspace,
    process::identity,
    test_support::{git, manager, repository},
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

#[tokio::test]
async fn directory_activity_distinguishes_inspection_from_removal_checks() {
    for (removal, scans_directory) in [
        (
            Removal::Manual {
                choice: BranchChoice::Auto,
                inspection: InspectionPolicy::GitOnly,
            },
            false,
        ),
        (
            Removal::Manual {
                choice: BranchChoice::Auto,
                inspection: InspectionPolicy::IncludeDirectoryProcesses,
            },
            true,
        ),
        (Removal::Automatic { snapshot: 0 }, true),
        (Removal::Merged { head: "" }, true),
    ] {
        let (_root, manager, workspace) = fixture().await;
        let mut child = tokio::process::Command::new("sleep")
            .arg("60")
            .env_clear()
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
                choice: BranchChoice::KeepBranch,
                inspection: InspectionPolicy::GitOnly,
            },
            Removal::Automatic { snapshot: 0 },
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
            let error = manager.stop_workspace(&workspace.id).await.unwrap_err();
            assert!(
                error.to_string().contains("ownership is incomplete"),
                "{error:#}"
            );
            let result = manager.remove(&workspace.id, removal).await;
            let retained = matches!(removal, Removal::Automatic { .. } | Removal::Deleted);
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
        let error = manager.stop_workspace(&workspace.id).await.unwrap_err();
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
                choice: BranchChoice::Auto,
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
