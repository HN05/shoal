use crate::cleanup::*;
use crate::model::CleanupReason;
use crate::model::Workspace;
use crate::stand_in::Fixture;
use crate::workspace_files::Attacher;
use shoal_protocol::assignments::Done;
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
    // Filesystem timestamps are coarse: rewrite until the change time moves.
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

/// A registered repository, a parent at an explicit path in another directory
/// than the repository's workspaces, and a child of it there.
async fn split_across_directories(manager: &Fixture) -> (Workspace, Workspace) {
    let repo = manager.repository("repo").await;
    let elsewhere = manager.root.path().join("elsewhere").join("parent");
    let parent = manager
        .create_workspace(&repo.id, "parent".into(), None, None, Some(elsewhere))
        .await
        .unwrap();
    let child = manager
        .spaces
        .create_child(
            &**manager,
            &SpaceSelector::id(&parent.id),
            "child".into(),
            None,
            Default::default(),
        )
        .await
        .unwrap();
    assert_ne!(parent.path.parent(), child.path.parent());
    (parent, child)
}

/// A chat with a workspace below it, whose agent runs.
async fn chat_with_workspace(manager: &Fixture) -> (crate::model::Chat, Workspace) {
    let repo = manager.repository("repo").await;
    let chat = manager.chat("talk", "chat", Some(&repo), None).await;
    manager.run_agent(&chat.id).await;
    let opened = manager
        .spaces
        .create_workspace_below(
            &**manager,
            repo.id.into(),
            "opened".into(),
            None,
            Default::default(),
            Some(chat.id.clone()),
        )
        .await
        .unwrap();
    (chat, opened)
}

#[tokio::test]
async fn deleted_parent_cleanup_keeps_a_parent_whose_child_elsewhere_stalled() {
    let manager = Fixture::new().await;
    let (parent, child) = split_across_directories(&manager).await;
    fs::remove_dir_all(&parent.path).unwrap();
    manager.spaces.file_access.stall_rechecking(&child.path);

    manager.sweep(&mut Timers::default()).await.unwrap();

    let remaining: Vec<_> = manager
        .spaces
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
async fn a_child_keeps_while_its_completed_parent_elsewhere_stalled() {
    let manager = Fixture::new().await;
    let (parent, child) = split_across_directories(&manager).await;
    manager
        .spaces
        .mark_space_done(
            &*manager,
            &SpaceSelector::id(&parent.id),
            Done {
                cleanup: true,
                ..Done::default()
            },
            &Attacher::User,
        )
        .await
        .unwrap();
    manager.spaces.file_access.stall_rechecking(&parent.path);

    manager.sweep(&mut Timers::default()).await.unwrap();

    // The parent's completion cannot be read, so it takes neither.
    assert!(parent.path.exists());
    assert!(child.path.exists());
}

#[tokio::test]
async fn a_stalled_directory_below_a_done_chat_starts_no_grace_period() {
    let manager = Fixture::new().await;
    let (chat, opened) = chat_with_workspace(&manager).await;
    manager
        .write_local_config(
            opened.repository(),
            Some("[cleanup]\ngrace_minutes = 600\n".into()),
        )
        .await
        .unwrap();
    let completion = manager
        .spaces
        .mark_space_done(
            &*manager,
            &SpaceSelector::id(&chat.id),
            Done {
                cleanup: true,
                ..Done::default()
            },
            &Attacher::User,
        )
        .await
        .unwrap();
    let chat = manager.spaces.chat_by_id(&chat.id).await.unwrap().unwrap();
    manager.spaces.file_access.stall_rechecking(&opened.path);
    let removed = manager
        .spaces
        .remove_completed_by(&*manager, &chat.id, &completion)
        .await;
    assert!(removed.is_err());
    assert_eq!(manager.spaces.cleanup_at(&chat.id).await.unwrap(), None);
    // Once access recovers, the grace period starts, and only then runs.
    manager.spaces.file_access.recover_at(&opened.path);
    assert!(
        !manager
            .spaces
            .remove_completed_by(&*manager, &chat.id, &completion)
            .await
            .unwrap()
    );
    assert!(manager.spaces.cleanup_at(&chat.id).await.unwrap().is_some());
    assert!(manager.spaces.workspace_by_id(&opened.id).await.is_ok());
}

#[tokio::test]
async fn a_done_chat_something_keeps_still_takes_the_workspaces_below_it() {
    let manager = Fixture::new().await;
    let (chat, opened) = chat_with_workspace(&manager).await;
    manager
        .spaces
        .mark_space_done(
            &*manager,
            &SpaceSelector::id(&chat.id),
            Done {
                cleanup: true,
                ..Done::default()
            },
            &Attacher::User,
        )
        .await
        .unwrap();
    // A question asked after the completion keeps the chat.
    manager.ask(&chat.id, "Which database?").await;

    manager.spaces.sweep_completed(&*manager).await.unwrap();
    assert!(manager.spaces.chat_by_id(&chat.id).await.unwrap().is_some());
    assert!(manager.spaces.workspace_by_id(&opened.id).await.is_err());
    assert!(!opened.path.exists());
}
