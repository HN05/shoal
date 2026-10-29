use super::*;
use crate::test_support::{manager, repository};
use std::sync::atomic::{AtomicUsize, Ordering};

#[tokio::test]
async fn cleanup_deletes_xcode_data_of_removed_workspaces_first() {
    let (root, manager) = manager().await;
    crate::test_support::configure(
        &manager,
        "presets = ['xcode']\n[cleanup]\ngrace_minutes = 0\n",
    )
    .await;
    let checkout = repository(root.path(), "repo");
    let repo = manager
        .register_repository(checkout.to_str().unwrap().into(), None, None)
        .await
        .unwrap();
    let live = manager
        .create_workspace(&repo.id, "live".into(), None, None, None)
        .await
        .unwrap();
    // A live worktree's project may be missing while it regenerates.
    let project = live.path.join("App.xcodeproj");
    let location = crate::presets::derived_data::location(&manager.paths.home).unwrap();
    let folder =
        |name, project: &Path| crate::presets::derived_data::folder(&location, name, project);
    let live = folder("App-live", &project);
    let directory = repo.workspaces_dir.as_deref().unwrap();
    let removed = folder("App-removed", &directory.join("gone/App.xcodeproj"));
    let elsewhere = folder("App-elsewhere", Path::new("/nonexistent/App.xcodeproj"));
    let settings = manager.repository_settings(&repo.id).await.unwrap();
    let cache =
        &crate::caches::directories(&repo, &settings.caches).unwrap()[crate::presets::XCODE_CACHE];
    let unpinned = cache.join("SourcePackages/unpinned");
    std::fs::create_dir_all(&unpinned).unwrap();
    let unused = std::time::SystemTime::now() - std::time::Duration::from_secs(2 * 86_400);
    std::fs::File::open(&unpinned)
        .unwrap()
        .set_modified(unused)
        .unwrap();
    let readings = AtomicUsize::new(0);
    let low = manager.config().overload.disk.cleanup_free_bytes() - 1;
    let available = |_: &Path| {
        // Deleting the Xcode data frees enough space.
        Ok(if readings.fetch_add(1, Ordering::Relaxed) == 0 {
            low
        } else {
            u64::MAX
        })
    };
    Monitor::default()
        .check(&manager, Instant::now(), &available)
        .await
        .unwrap();
    assert!(!removed.exists());
    assert!(live.exists());
    assert!(elsewhere.exists());
    assert!(!unpinned.exists());
    assert_eq!(manager.list_workspaces().await.unwrap().len(), 1);
}

#[tokio::test]
async fn cleanup_removes_candidates_until_enough_space_is_free() {
    let (root, manager) = manager().await;
    let checkout = repository(root.path(), "repo");
    let repo = manager
        .register_repository(checkout.to_str().unwrap().into(), None, None)
        .await
        .unwrap();
    for name in ["first", "second", "third"] {
        manager
            .create_workspace(&repo.id, name.into(), None, None, None)
            .await
            .unwrap();
    }
    let held = manager.workspace("first").await.unwrap();
    manager
        .acquire_hold(
            &held.id,
            "keep".into(),
            None,
            crate::model::HoldPlacer::User,
        )
        .await
        .unwrap();
    let readings = AtomicUsize::new(0);
    let low = manager.config().overload.disk.cleanup_free_bytes() - 1;
    let available = |_: &Path| {
        // The first removal frees enough space.
        Ok(if readings.fetch_add(1, Ordering::Relaxed) == 0 {
            low
        } else {
            u64::MAX
        })
    };
    let start = Instant::now();
    let mut monitor = Monitor::default();
    monitor.check(&manager, start, &available).await.unwrap();
    let names = |workspaces: Vec<Workspace>| {
        workspaces
            .into_iter()
            .map(|workspace| workspace.name)
            .collect::<Vec<_>>()
    };
    assert_eq!(
        names(manager.list_workspaces().await.unwrap()),
        ["first", "third"]
    );
    // That pass freed enough space, so the next low reading starts another.
    // Space above the stop threshold stops nothing.
    let above_stop = |_: &Path| Ok(manager.config().overload.disk.stop_free_bytes());
    monitor.check(&manager, start, above_stop).await.unwrap();
    assert_eq!(names(manager.list_workspaces().await.unwrap()), ["first"]);
    // A pass that could not free enough space waits for the interval.
    manager
        .release_hold(&held.id, "keep".into(), crate::model::HoldPlacer::User)
        .await
        .unwrap();
    monitor.check(&manager, start, above_stop).await.unwrap();
    assert_eq!(names(manager.list_workspaces().await.unwrap()), ["first"]);
    monitor
        .check(&manager, start + CLEANUP_INTERVAL, above_stop)
        .await
        .unwrap();
    assert!(manager.list_workspaces().await.unwrap().is_empty());

    // Agents recover only once every filesystem has the cleanup threshold.
    let cleanup = manager.config().overload.disk.cleanup_free_bytes();
    let recovered = |manager: &Manager| manager.config_state.disk_space_recovered();
    monitor
        .check(&manager, start, |_: &Path| Ok(cleanup - 1))
        .await
        .unwrap();
    assert!(!recovered(&manager));
    monitor
        .check(&manager, start, |_: &Path| Ok(cleanup))
        .await
        .unwrap();
    assert!(recovered(&manager));
    // A reload, even back to the same settings, waits for a new reading,
    // including one published after the reload from an earlier snapshot.
    let sampled = manager.config_state.generation();
    manager.reload_config().await.unwrap();
    assert!(!recovered(&manager));
    manager.config_state.publish_disk_reading(Some(sampled));
    assert!(!recovered(&manager));
    monitor
        .check(&manager, start, |_: &Path| Ok(cleanup))
        .await
        .unwrap();
    assert!(recovered(&manager));
    monitor
        .check(&manager, start, |_: &Path| {
            Err(io::Error::other("unreadable"))
        })
        .await
        .unwrap();
    assert!(!recovered(&manager));

    // Unreadable daemon state is never a healthy reading, and only a
    // deleted workspace is skipped without failing the sample.
    let unreadable = |path: &Path| sample(path, &[], |_: &Path| Ok(u64::MAX));
    let (filesystems, complete) = unreadable(&root.path().join("missing"));
    assert!(filesystems.is_empty() && !complete);
    let mut workspace = held.clone();
    workspace.path = root.path().join("deleted");
    let (_, complete) = sample(root.path(), &[workspace.clone()], |_: &Path| Ok(u64::MAX));
    assert!(complete);
    let file = root.path().join("file");
    std::fs::write(&file, "").unwrap();
    workspace.path = file.join("below-a-file");
    let (_, complete) = sample(root.path(), &[workspace], |_: &Path| Ok(u64::MAX));
    assert!(!complete);

    // Disabled protection reads nothing and forgets its last reading.
    manager
        .config_state
        .publish_disk_reading(Some(manager.config_state.generation()));
    crate::test_support::configure(&manager, "[overload.disk]\nenabled = false\n").await;
    Monitor::default()
        .check(&manager, start, |_: &Path| -> io::Result<u64> {
            panic!("disabled disk protection sampled free space")
        })
        .await
        .unwrap();
    assert!(!recovered(&manager));
}

#[tokio::test]
async fn cleanup_empties_unused_caches_after_removing_workspaces() {
    let (root, manager) = manager().await;
    let checkout = repository(root.path(), "repo");
    let repo = manager
        .register_repository(checkout.to_str().unwrap().into(), None, None)
        .await
        .unwrap();
    let held = manager
        .create_workspace(&repo.id, "held".into(), None, None, None)
        .await
        .unwrap();
    manager
        .acquire_hold(
            &held.id,
            "keep".into(),
            None,
            crate::model::HoldPlacer::User,
        )
        .await
        .unwrap();
    let repo = manager.repository(&repo.id).await.unwrap();
    let caches = std::collections::BTreeMap::from([(
        "go".to_owned(),
        crate::config::repo::Cache {
            scope: crate::config::repo::CacheScope::Shared,
        },
    )]);
    let go = crate::caches::directories(&repo, &caches).unwrap()["go"].clone();
    std::fs::write(go.join("entry"), "cached").unwrap();
    // Above the stop threshold, so only cleanup runs.
    let low = manager.config().overload.disk.cleanup_free_bytes() - 1;
    Monitor::default()
        .check(&manager, Instant::now(), |_: &Path| Ok(low))
        .await
        .unwrap();
    assert!(!go.exists());
    assert_eq!(manager.list_workspaces().await.unwrap().len(), 1);
    let notifications = manager.notifications(true, 10).await.unwrap();
    let emptied = notifications
        .iter()
        .find(|notification| notification.kind == NotificationKind::CachesEmptied)
        .expect("caches emptied notification");
    assert!(
        emptied
            .message
            .starts_with("Emptied caches of repo for disk space: go; "),
        "{}",
        emptied.message
    );
}

#[tokio::test]
async fn cleanup_deletes_the_results_of_removed_workspaces_last() {
    let (root, manager) = manager().await;
    let checkout = repository(root.path(), "repo");
    let repo = manager
        .register_repository(checkout.to_str().unwrap().into(), None, None)
        .await
        .unwrap();
    let audit = manager
        .create_workspace(&repo.id, "audit".into(), None, None, None)
        .await
        .unwrap();
    let report = root.path().join("report.md");
    std::fs::write(&report, "No leaks").unwrap();
    manager
        .mark_done(&audit.id, false, false, vec![report])
        .await
        .unwrap();
    let result = manager.workspace_files(&audit.id).await.unwrap()[0]
        .path
        .clone();
    manager
        .remove_workspace(
            &audit.id,
            crate::removal::UnsavedWork::default(),
            crate::removal::InspectionPolicy::GitOnly,
        )
        .await
        .unwrap();
    assert!(result.exists(), "removal keeps results");
    // Above the stop threshold, so only cleanup runs.
    let low = manager.config().overload.disk.cleanup_free_bytes() - 1;
    Monitor::default()
        .check(&manager, Instant::now(), |_: &Path| Ok(low))
        .await
        .unwrap();
    assert!(!result.exists());
    let notifications = manager.notifications(true, 10).await.unwrap();
    let deleted = notifications
        .iter()
        .find(|notification| notification.kind == NotificationKind::ResultsDeleted)
        .expect("results deleted notification");
    assert!(
        deleted.message.starts_with(
            "Deleted the results removed workspace audit handed back, for disk space; "
        ),
        "{}",
        deleted.message
    );
}

#[tokio::test]
async fn cleanup_reads_space_again_after_purging_leftover_caches() {
    let (root, manager) = manager().await;
    let checkout = repository(root.path(), "repo");
    let repo = manager
        .register_repository(checkout.to_str().unwrap().into(), None, None)
        .await
        .unwrap();
    let held = manager
        .create_workspace(&repo.id, "held".into(), None, None, None)
        .await
        .unwrap();
    manager
        .acquire_hold(
            &held.id,
            "keep".into(),
            None,
            crate::model::HoldPlacer::User,
        )
        .await
        .unwrap();
    let repo = manager.repository(&repo.id).await.unwrap();
    let caches = std::collections::BTreeMap::from([(
        "go".to_owned(),
        crate::config::repo::Cache {
            scope: crate::config::repo::CacheScope::Shared,
        },
    )]);
    crate::caches::directories(&repo, &caches).unwrap();
    // A pass interrupted after moving the caches aside.
    let layout = Layout::of(&repo).unwrap();
    layout.move_aside().unwrap();
    let readings = AtomicUsize::new(0);
    let critical = manager.config().overload.disk.stop_free_bytes() - 1;
    let available = |_: &Path| {
        // Purging the leftovers frees enough space.
        Ok(if readings.fetch_add(1, Ordering::Relaxed) == 0 {
            critical
        } else {
            u64::MAX
        })
    };
    Monitor::default()
        .check(&manager, Instant::now(), &available)
        .await
        .unwrap();
    assert!(!layout.root().join("trash").exists());
    // The stop decision used the reading taken after the purge.
    assert_eq!(readings.load(Ordering::Relaxed), 2);
}

#[tokio::test]
async fn failed_cleanup_still_stops_executions_on_critical_space() {
    let (root, manager) = manager().await;
    let checkout = repository(root.path(), "repo");
    let repo = manager
        .register_repository(checkout.to_str().unwrap().into(), None, None)
        .await
        .unwrap();
    let busy = manager
        .create_workspace(&repo.id, "busy".into(), None, None, None)
        .await
        .unwrap();
    for name in ["first", "second"] {
        manager
            .create_workspace(&repo.id, name.into(), None, None, None)
            .await
            .unwrap();
    }
    manager
        .begin_execution(
            &busy.id,
            None,
            crate::daemon::workspace::ExecutionKind::Command,
            None,
        )
        .await
        .unwrap();
    let settings = &manager.config().overload.disk;
    let (critical, low) = (
        settings.stop_free_bytes() - 1,
        settings.cleanup_free_bytes() - 1,
    );
    let remaining = || async { manager.list_workspaces().await.unwrap().len() };
    let stops = || async {
        manager
            .notifications(true, 10)
            .await
            .unwrap()
            .into_iter()
            .filter(|notification| {
                matches!(
                    notification.kind,
                    NotificationKind::AgentStopped | NotificationKind::StopFailed
                )
            })
            .count()
    };
    let start = Instant::now();

    // Every reading after the first removal fails, so nothing stops.
    let readings = AtomicUsize::new(0);
    let unreadable = |_: &Path| match readings.fetch_add(1, Ordering::Relaxed) {
        0 => Ok(critical),
        _ => Err(io::Error::other("unreadable")),
    };
    let mut monitor = Monitor::default();
    monitor.check(&manager, start, &unreadable).await.unwrap();
    assert_eq!(remaining().await, 2);
    assert_eq!(stops().await, 0);
    // The failed pass waits for the interval like one that freed too little.
    monitor
        .check(&manager, start, |_: &Path| Ok(low))
        .await
        .unwrap();
    assert_eq!(remaining().await, 2);

    // A fresh reading after the failure still stops executions.
    let readings = AtomicUsize::new(0);
    let recovered = |_: &Path| match readings.fetch_add(1, Ordering::Relaxed) {
        1 => Err(io::Error::other("unreadable")),
        _ => Ok(critical),
    };
    Monitor::default()
        .check(&manager, start, &recovered)
        .await
        .unwrap();
    assert_eq!(remaining().await, 1);
    assert_eq!(stops().await, 1);
}

#[tokio::test]
async fn low_space_warns_running_agents_before_cleanup_starts() {
    let (root, manager) = manager().await;
    let checkout = repository(root.path(), "repo");
    let repo = manager
        .register_repository(checkout.to_str().unwrap().into(), None, None)
        .await
        .unwrap();
    let workspace = manager
        .create_workspace(&repo.id, "agent".into(), None, None, None)
        .await
        .unwrap();
    let execution = manager
        .begin_execution(
            &workspace.id,
            None,
            crate::daemon::workspace::ExecutionKind::Command,
            None,
        )
        .await
        .unwrap();
    manager
        .track_agent(
            &execution.plan.id,
            "claude",
            &workspace.id,
            &workspace.name,
            false,
            Duration::ZERO,
        )
        .await;
    let config = manager.config();
    let above_warning = config.overload.warning.disk_free_bytes();
    let mut monitor = Monitor::default();
    monitor
        .check(&manager, Instant::now(), |_: &Path| Ok(above_warning))
        .await
        .unwrap();
    assert!(
        manager
            .agent_messages(&workspace.id)
            .await
            .unwrap()
            .is_empty()
    );
    let low = above_warning - 1;
    monitor
        .check(&manager, Instant::now(), |_: &Path| Ok(low))
        .await
        .unwrap();
    let messages = manager.agent_messages(&workspace.id).await.unwrap();
    assert_eq!(messages.len(), 1);
    assert!(
        messages[0]
            .message
            .starts_with("Free disk space is low: 10.0 GiB available at "),
        "{}",
        messages[0].message
    );
    // Space above the cleanup threshold removes nothing.
    assert_eq!(manager.list_workspaces().await.unwrap().len(), 1);
}
