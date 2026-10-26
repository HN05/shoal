use crate::{
    presets::{self, derived_data, xcode},
    removal::{InspectionPolicy, UnsavedWork},
    test_support::{commit, configure, manager, repository},
};
use std::{fs, time::SystemTime};

#[tokio::test]
async fn cleanup_skips_a_busy_repository_and_recorded_worktrees() {
    let (root, manager) = manager().await;
    configure(
        &manager,
        "presets = ['xcode']\n[cleanup]\ngrace_minutes = 0\n",
    )
    .await;
    let checkout = repository(root.path(), "repo");
    let repo = manager
        .register_repository(checkout.to_str().unwrap().into(), None, None)
        .await
        .unwrap();
    let mut worktrees = Vec::new();
    for name in ["removed", "moved"] {
        let workspace = manager
            .create_workspace(&repo.id, name.into(), None, None, None)
            .await
            .unwrap();
        worktrees.push(workspace.path);
    }
    // A worktree moved outside Shoal keeps its record and DerivedData.
    fs::rename(&worktrees[1], root.path().join("moved")).unwrap();
    let location = derived_data::location(&manager.paths.home).unwrap();
    let directory = repo.workspaces_dir.clone().unwrap();
    let left = derived_data::folder(&location, "App-gone", &directory.join("gone/App"));
    let moved = derived_data::folder(&location, "App-moved", &worktrees[1].join("App"));
    let settings = manager.repository_settings(&repo.id).await.unwrap();
    let cache = crate::caches::directories(&repo, &settings.caches).unwrap()[presets::XCODE_CACHE]
        .join("SourcePackages/unpinned");
    fs::create_dir_all(&cache).unwrap();
    let old = SystemTime::now() - 2 * super::CHECKOUTS_UNUSED_FOR;
    fs::File::open(&cache).unwrap().set_modified(old).unwrap();

    // Creating a workspace or landing holds the gate.
    let gate = manager.lock_repository_git(&repo.id).await;
    manager
        .remove_workspace("removed", UnsavedWork::default(), InspectionPolicy::GitOnly)
        .await
        .unwrap();
    assert!(!manager.delete_xcode_leftovers(&repo, |_| true).await);
    assert!(left.exists());
    assert!(cache.exists());
    drop(gate);
    assert!(manager.delete_xcode_leftovers(&repo, |_| true).await);
    assert!(!left.exists());
    assert!(!cache.exists());
    assert!(moved.exists());
}

#[tokio::test]
async fn a_pin_file_that_cannot_be_read_stops_pruning() {
    let root = tempfile::tempdir().unwrap();
    let worktree = repository(root.path(), "repo");
    fs::write(worktree.join(xcode::PINS), "pins").unwrap();
    assert_eq!(super::worktree_pins(&worktree).await.unwrap(), [b"pins"]);
    // Reading a directory fails even for root.
    fs::remove_file(worktree.join(xcode::PINS)).unwrap();
    std::os::unix::fs::symlink(".", worktree.join(xcode::PINS)).unwrap();
    assert!(super::worktree_pins(&worktree).await.is_err());
}

#[tokio::test]
async fn removal_prunes_package_checkouts_nothing_pins() {
    let (root, manager) = manager().await;
    configure(
        &manager,
        "presets = ['xcode']\n[cleanup]\ngrace_minutes = 0\n",
    )
    .await;
    let checkout = repository(root.path(), "repo");
    fs::write(checkout.join(xcode::PINS), "main").unwrap();
    commit(&checkout, xcode::PINS);
    let repo = manager
        .register_repository(checkout.to_str().unwrap().into(), None, None)
        .await
        .unwrap();
    for name in ["pinned", "removed"] {
        let workspace = manager
            .create_workspace(&repo.id, name.into(), None, None, None)
            .await
            .unwrap();
        // A generated project is often ignored, with its pins.
        let generated = workspace
            .path
            .join("App.xcodeproj/project.xcworkspace/xcshareddata/swiftpm");
        fs::create_dir_all(&generated).unwrap();
        fs::write(generated.join(xcode::PINS), name).unwrap();
        fs::write(workspace.path.join(".gitignore"), "App.xcodeproj/\n").unwrap();
    }
    let settings = manager.repository_settings(&repo.id).await.unwrap();
    let cache =
        crate::caches::directories(&repo, &settings.caches).unwrap()[presets::XCODE_CACHE].clone();
    let checkouts = |pins: &str| {
        cache
            .join("SourcePackages")
            .join(xcode::checkouts_key(pins.as_bytes()))
    };
    let old = SystemTime::now() - 2 * super::CHECKOUTS_UNUSED_FOR;
    for pins in ["main", "pinned", "removed", "recent"] {
        fs::create_dir_all(checkouts(pins)).unwrap();
        if pins != "recent" {
            let directory = fs::File::open(checkouts(pins)).unwrap();
            directory.set_modified(old).unwrap();
        }
    }
    manager
        .remove_workspace("removed", UnsavedWork::All, InspectionPolicy::GitOnly)
        .await
        .unwrap();
    assert!(!checkouts("removed").exists());
    for kept in ["main", "pinned", "recent"] {
        assert!(checkouts(kept).exists(), "{kept} kept");
    }
}

#[tokio::test]
async fn removal_deletes_the_workspaces_derived_data_under_the_xcode_preset() {
    let (root, manager) = manager().await;
    configure(
        &manager,
        "presets = ['xcode']\n[cleanup]\ngrace_minutes = 0\n",
    )
    .await;
    let checkout = repository(root.path(), "repo");
    let repo = manager
        .register_repository(checkout.to_str().unwrap().into(), None, None)
        .await
        .unwrap();
    let location = derived_data::location(&manager.paths.home).unwrap();
    let mut folders = Vec::new();
    for name in ["first", "second"] {
        let workspace = manager
            .create_workspace(&repo.id, name.into(), None, None, None)
            .await
            .unwrap();
        let project = workspace.path.join("app/App.xcodeproj");
        folders.push(derived_data::folder(
            &location,
            &format!("App-{name}"),
            &project,
        ));
    }
    let remove =
        |name| manager.remove_workspace(name, UnsavedWork::default(), InspectionPolicy::GitOnly);
    remove("first").await.unwrap();
    assert!(!folders[0].exists());
    assert!(folders[1].exists());
    configure(&manager, "[cleanup]\ngrace_minutes = 0\n").await;
    remove("second").await.unwrap();
    assert!(folders[1].exists(), "kept without the preset");
}
