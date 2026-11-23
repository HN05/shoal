use crate::cleanup::*;
use crate::model::CleanupReason;
use crate::stand_in::Fixture;
use std::fs;
use std::os::unix::fs::MetadataExt;
use std::time::Duration;
use tokio::time::Instant;

use shoal_ids::space_ids::SpaceId;
use shoal_protocol::space_names::SpaceSelector;

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
        timers.expired(&SpaceId::from("w"), 1, now, delay),
        Expiry::Restart
    ));
    timers.start(&SpaceId::from("w"), snapshot, now);
    assert!(matches!(
        timers.expired(&SpaceId::from("w"), 1, now + delay / 2, delay),
        Expiry::Running
    ));
    assert!(matches!(
        timers.expired(&SpaceId::from("w"), 2, now + delay, delay),
        Expiry::Restart
    ));
    assert!(matches!(
        timers.expired(&SpaceId::from("w"), 1, now + delay, delay),
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
async fn requested_cleanup_removes_candidates_without_idle_delay() {
    use crate::stand_in::repository;
    let manager = Fixture::new().await;
    let temp = &manager.root;
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
    let broken = manager
        .spaces
        .workspace(&manager.named("broken").await)
        .await
        .unwrap();
    fs::remove_file(broken.path.join(".git")).unwrap();
    let dirty = manager
        .spaces
        .workspace(&manager.named("dirty").await)
        .await
        .unwrap();
    fs::write(dirty.path.join("work"), "retain me").unwrap();
    let held = manager
        .spaces
        .workspace(&manager.named("held").await)
        .await
        .unwrap();
    manager
        .spaces
        .acquire_hold(
            &*manager,
            &SpaceSelector::id(&held.id),
            "keep".into(),
            None,
            crate::model::HoldPlacer::User,
        )
        .await
        .unwrap();
    let names = |manager: &Fixture| {
        let manager = manager.daemon.clone();
        async move {
            manager
                .spaces
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
            .map(|retained| retained.space.clone())
            .collect::<Vec<_>>()
    };

    let all = Selection {
        spaces: Pick::All,
        ..Selection::default()
    };
    let preview = on_request(&*manager, true, &all).await.unwrap();
    assert_eq!(preview.removed, ["idle"]);
    assert_eq!(retained(&preview), ["broken"]);
    let kept = preview
        .kept
        .iter()
        .map(|kept| (kept.space.as_str(), kept.reason.clone()))
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
        spaces: Pick::Only(vec![dirty.id.to_string(), held.id.to_string()]),
        ..Selection::default()
    };
    let report = on_request(&*manager, false, &named).await.unwrap();
    assert!(report.removed.is_empty());
    assert_eq!(
        report
            .kept
            .iter()
            .map(|kept| kept.space.as_str())
            .collect::<Vec<_>>(),
        ["dirty", "held"]
    );
    let unknown = Selection {
        spaces: Pick::Only(vec!["missing".into()]),
        ..Selection::default()
    };
    assert!(on_request(&*manager, false, &unknown).await.is_err());
    assert_eq!(names(&manager).await, ["broken", "dirty", "held", "idle"]);

    let report = on_request(&*manager, false, &all).await.unwrap();
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
async fn a_cleanup_request_takes_one_of_two_same_named_spaces() {
    use crate::stand_in::repository;
    let manager = Fixture::new().await;
    let temp = &manager.root;
    let checkout = repository(temp.path(), "repo");
    let repo = manager
        .register_repository(checkout.to_str().unwrap().into(), None, None)
        .await
        .unwrap();
    let mut workspaces = Vec::new();
    for branch in ["first", "second"] {
        let workspace = manager
            .create_workspace(&repo.id, branch.into(), None, None, None)
            .await
            .unwrap();
        (manager
            .spaces
            .rename_space(&SpaceSelector::id(&workspace.id), &"Same".parse().unwrap()))
        .await
        .unwrap();
        workspaces.push(workspace);
    }
    // A name selects nothing; the ID selects its space alone.
    let by_name = Selection {
        spaces: Pick::Only(vec!["Same".into()]),
        ..Selection::default()
    };
    assert!(on_request(&*manager, false, &by_name).await.is_err());
    let first = Selection {
        spaces: Pick::Only(vec![workspaces[0].id.to_string()]),
        ..Selection::default()
    };
    let report = on_request(&*manager, false, &first).await.unwrap();
    assert_eq!(report.removed, ["Same"]);
    assert_eq!(report.removed_ids, [workspaces[0].id.clone()]);
    let left: Vec<_> = (manager.spaces.list_workspaces().await.unwrap().into_iter())
        .map(|workspace| workspace.id)
        .collect();
    assert_eq!(left, [workspaces[1].id.clone()]);
}
