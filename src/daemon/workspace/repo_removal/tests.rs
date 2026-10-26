use super::*;
use crate::daemon::workspace::WorkspaceSource;
use crate::test_support;
use std::os::unix::fs::MetadataExt;

#[tokio::test]
async fn removal_waits_for_queued_workspace_creation() {
    let (root, manager) = test_support::manager().await;
    let checkout = test_support::repository(root.path(), "repo");
    let repo = manager
        .register_repository(checkout.to_str().unwrap().into(), None, None)
        .await
        .unwrap();

    let gate = manager.lock_repository_git(&repo.id).await;
    let creating_manager = manager.clone();
    let creating_repo = repo.clone();
    let mut creating = Box::pin(async move {
        let _git = creating_manager
            .lock_repository_git(&creating_repo.id)
            .await;
        creating_manager
            .create_branch_workspace(
                &creating_repo,
                "racing".into(),
                "racing".into(),
                WorkspaceSource::New {
                    base: None,
                    parent: None,
                },
                None,
                None,
            )
            .await
    });
    test_support::assert_pending(&mut creating).await;

    // Shared caches go with the repository directory.
    let caches = std::collections::BTreeMap::from([(
        "go".to_owned(),
        crate::config::repo::Cache {
            scope: crate::config::repo::CacheScope::Shared,
        },
    )]);
    crate::caches::directories(&repo, &caches).unwrap();
    let mut removing = Box::pin(manager.remove_repository(&repo.id));
    test_support::assert_pending(&mut removing).await;
    drop(gate);

    let workspace = creating.await.unwrap();
    let removal = removing.await.unwrap();
    assert_eq!(removal.workspaces_removed, 1);
    assert!(!workspace.path.exists());
    assert!(manager.repositories().await.unwrap().is_empty());
    assert!(manager.list_workspaces().await.unwrap().is_empty());
    assert!(!repo.path.exists());
    assert!(!repo.workspaces_dir.unwrap().exists());
}

#[tokio::test]
async fn removal_names_a_bare_flag_on_a_checkout_with_a_working_tree() -> Result<()> {
    let (root, manager) = test_support::manager().await;
    let checkout = test_support::repository(root.path(), "repo");
    let repo = manager
        .register_repository(checkout.to_string_lossy().into_owned().into(), None, None)
        .await?;
    test_support::git(&repo.path, &["config", "core.bare", "true"]);

    let error = manager
        .remove_repository(&repo.id)
        .await
        .err()
        .context("removal succeeded")?;
    let message = format!("{error:#}");
    assert!(message.contains("core.bare=true"), "{message}");
    assert!(message.contains("config core.bare false"), "{message}");
    assert!(repo.path.exists());

    test_support::git(&repo.path, &["config", "core.bare", "false"]);
    manager.remove_repository(&repo.id).await?;
    assert!(!repo.path.exists());
    Ok(())
}

#[test]
fn recorded_identity_verifies_including_legacy_device_inode() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let root = fs::canonicalize(temp.path())?;
    let directory = root.join("checkout");
    fs::create_dir(&directory)?;
    let metadata = fs::metadata(&directory)?;
    let legacy = format!("{}:{}", metadata.dev(), metadata.ino());
    let current = real_directory_identity(&directory)?.context("missing identity")?;
    for recorded in [current.as_str(), legacy.as_str()] {
        verify_directory(&directory, Some(recorded), false)?;
    }
    assert!(verify_directory(&directory, None, true).is_err());

    let moved = root.join("moved");
    fs::rename(&directory, &moved)?;
    fs::create_dir(&directory)?;
    for recorded in [current.as_str(), legacy.as_str()] {
        assert!(verify_directory(&directory, Some(recorded), true).is_err());
    }
    fs::remove_dir(&directory)?;
    verify_directory(&directory, Some(&current), true)?;
    verify_directory(&directory, None, false)?;
    assert!(verify_directory(&directory, Some(&current), false).is_err());
    Ok(())
}
