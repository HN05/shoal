use shoal_ids::execution_ids::ExecutionId;
use std::fs;

use super::Import;
use super::transfer::{exporter, import, importer, packaged_workspace};
use crate::daemon::notifications::AlertKind;
use crate::daemon::restore::SpaceImport;
use crate::daemon::spaces::MovePacking;
use crate::protocol::operations::{Ended, OperationKind, Outcome};
use crate::refusal::Reason;
use crate::{
    daemon::{
        manager::Manager,
        test_support::{self, commit, install_test_script},
    },
    execution::recovery,
    model::Workspace,
    process::identity,
    removal::UnsavedWork,
    transfer::{
        Carrying, ExportedSpace, ManifestExt, MoveDestination, MoveFailure, MoveLock, MovePhase,
        MoveRecord, MoveReport,
    },
};
use shoal_ids::checkpoint_numbers::CheckpointNumber;
use shoal_ids::space_ids::SpaceId;
use shoal_protocol::space_names::SpaceSelector;
use std::collections::BTreeMap;
use std::path::Path;

/// Export `space` for a move into a new file in `directory`, which may
/// delete nothing, and read it back.
async fn export_moving(
    manager: &Manager,
    space: &SpaceSelector,
    directory: &std::path::Path,
) -> (ExportedSpace, Vec<u8>) {
    export_deleting(manager, space, directory, UnsavedWork::default()).await
}

/// Export `space` for a move into a new file in `directory`, which may
/// delete the `unsaved` work, and read it back.
async fn export_deleting(
    manager: &Manager,
    space: &SpaceSelector,
    directory: &std::path::Path,
    unsaved: UnsavedWork,
) -> (ExportedSpace, Vec<u8>) {
    let file = directory.join("moving.shoal");
    let packing = MovePacking {
        unsaved,
        destination: MoveDestination::File { path: file.clone() },
        carrying: Carrying::Subtree,
        again: false,
    };
    let exported = manager.export_moving_space(space, packing).await.unwrap();
    (exported, fs::read(file).unwrap())
}

/// A workspace of the exporter whose agent stopped earlier, as its export
/// finds it.
async fn stopped_workspace(manager: &Manager, repo: &crate::model::Repository) -> Workspace {
    let workspace = manager
        .create_workspace(&repo.id, "topic".into(), None, None, None)
        .await
        .unwrap();
    fs::write(workspace.path.join("work"), "work\n").unwrap();
    commit(&workspace.path, "work");
    recovery::save_record(
        &manager.paths,
        &workspace.id,
        &ExecutionId::from("stopped-agent"),
        &shoal_protocol::executions::TrackedAgent::named("claude"),
        Some("overload"),
        None,
    )
    .unwrap();
    workspace
}

/// The checkpoint of each workspace the package of `exported` holds.
fn exported_checkpoints(exported: &ExportedSpace) -> BTreeMap<SpaceId, CheckpointNumber> {
    (exported.manifest.packaged())
        .map(|packaged| {
            let kept = packaged.record.kept_workspace().unwrap();
            (packaged.record.space_id.clone(), kept.checkpoint.number)
        })
        .collect()
}

/// Have the removal hook of `repo`'s workspaces write a file into each.
async fn write_on_removal(manager: &Manager, repo: &crate::model::Repository, root: &Path) {
    let hook = root.join("write-on-removal");
    install_test_script(&hook, "#!/bin/sh\nprintf 'hooked' > hooked\n");
    let config = format!("hooks.pre_remove = '{}'\n", hook.display());
    (manager.write_local_config(&repo.id, Some(config)))
        .await
        .unwrap();
}

#[tokio::test]
async fn a_moving_workspace_stays_until_its_import_ends_it() {
    let (source, exporting, source_repo, origin) = exporter().await;
    let (_target, importing, target_repo) = importer(&origin).await;
    let workspace = stopped_workspace(&exporting, &source_repo).await;
    let selector = SpaceSelector::id(&workspace.id);

    let (exported, file) = export_moving(&exporting, &selector, source.path()).await;
    assert!(workspace.path.join("work").exists());
    let packaged = packaged_workspace(&exported.manifest);
    assert_eq!(packaged.record.agents.len(), 1);
    let restored = import(&importing, &target_repo, &file).await;
    assert_eq!(
        fs::read_to_string(restored.opened.workspace.path.join("work")).unwrap(),
        "work\n"
    );

    let ended = exporting.end_moved_space(&selector).await.unwrap();
    assert!(ended.removed);
    assert!(!workspace.path.exists());
    let removed = exporting.removed_workspaces().await.unwrap();
    assert_eq!(removed[0].space_id, workspace.id);
    assert_eq!(removed[0].agents.len(), 1);
}

#[tokio::test]
async fn a_move_is_recorded_with_its_package_until_it_ends() {
    let (source, exporting, source_repo, _origin) = exporter().await;
    let workspace = stopped_workspace(&exporting, &source_repo).await;
    let selector = SpaceSelector::id(&workspace.id);

    let (exported, _) = export_moving(&exporting, &selector, source.path()).await;
    let record = recorded(&exporting, &workspace.id).await.unwrap();
    // The file is the destination, which has the package once it is written.
    assert_eq!(record.phase, MovePhase::Delivered);
    assert_eq!(record.checkpoints, exported_checkpoints(&exported));
    // The agent stopped earlier is no agent the move stopped.
    let saved = &record.saved_before[&workspace.id];
    assert!(saved.contains(&ExecutionId::from("stopped-agent")));
    // The space is the move's until it ends: another move is refused,
    // and so is packing again, which only waits for a move in its first
    // phase, before anything else can fail, such as a directory it cannot
    // write, and either leaves the move as it was.
    for again in [false, true] {
        let packing = MovePacking {
            unsaved: UnsavedWork::default(),
            destination: MoveDestination::File {
                path: source.path().join("missing").join("again.shoal"),
            },
            carrying: Carrying::Subtree,
            again,
        };
        let refused = exporting.export_moving_space(&selector, packing).await;
        assert_eq!(
            Reason::of(&refused.unwrap_err()),
            Some(Reason::MoveInProgress)
        );
        assert_eq!(
            recorded(&exporting, &workspace.id).await,
            Some(record.clone())
        );
    }

    exporting.end_moved_space(&selector).await.unwrap();
    assert!(recorded(&exporting, &workspace.id).await.is_none());
}

#[tokio::test]
async fn a_move_recorded_before_its_package_was_written_packs_again_under_its_record() {
    let (source, exporting, source_repo, _origin) = exporter().await;
    let workspace = stopped_workspace(&exporting, &source_repo).await;
    let destination = MoveDestination::File {
        path: source.path().join("moving.shoal"),
    };
    // As a daemon that stopped while it packed the space left it.
    let saved_before = [(workspace.id.clone(), ["earlier".into()].into())].into();
    let left = MoveRecord {
        space: workspace.id.clone(),
        destination: destination.clone(),
        ticket: shoal_ids::move_tickets::MoveTicket::generate(),
        carrying: Carrying::Subtree,
        unsaved: UnsavedWork::All,
        phase: MovePhase::Packing,
        checkpoints: Default::default(),
        saved_before,
    };
    let inserted = left.clone();
    (exporting.store)
        .write(move |tx| super::super::records::insert(tx, &inserted))
        .await
        .unwrap();

    let selector = SpaceSelector::id(&workspace.id);
    let packing = |again| MovePacking {
        unsaved: UnsavedWork::default(),
        destination: destination.clone(),
        carrying: Carrying::Subtree,
        again,
    };
    // Only the move's finisher packs again under its record.
    let refused = exporting
        .export_moving_space(&selector, packing(false))
        .await;
    assert_eq!(
        Reason::of(&refused.unwrap_err()),
        Some(Reason::MoveInProgress)
    );
    let exported = (exporting.export_moving_space(&selector, packing(true)))
        .await
        .unwrap();
    let record = recorded(&exporting, &workspace.id).await.unwrap();
    assert_eq!(record.phase, MovePhase::Delivered);
    assert_eq!(record.checkpoints, exported_checkpoints(&exported));
    // What the move's caller let it delete, and what it saved before, stay.
    assert_eq!(
        (record.unsaved, record.saved_before),
        (left.unsaved, left.saved_before)
    );
}

#[tokio::test]
async fn an_abandoned_move_returns_the_agents_it_stopped_and_ends() {
    let (root, manager, _pid) = super::chat("talk").await;
    let chat = manager.chats().await.unwrap().remove(0);
    crate::daemon::test_support::report_session(&manager, &chat.id, "s1").await;
    let selector = SpaceSelector::id(&chat.id);
    let (_, _) = export_moving(&manager, &selector, root.path()).await;

    let stopped = manager.abandon_move(&selector).await.unwrap();
    let saved = manager.saved_work(&selector).await.unwrap().execution_ids();
    assert_eq!(saved.len(), 1);
    assert_eq!(stopped[&chat.id].iter().collect::<Vec<_>>(), [&saved[0]]);
    assert!(recorded(&manager, &chat.id).await.is_none());
    assert_eq!(manager.chats().await.unwrap()[0].id, chat.id);
}

#[tokio::test]
async fn files_written_before_the_export_end_with_the_workspace_the_caller_saw_clean() {
    let (source, exporting, source_repo, _origin) = exporter().await;
    let workspace = stopped_workspace(&exporting, &source_repo).await;
    let selector = SpaceSelector::id(&workspace.id);
    // As an agent writes when it stops, after the caller's check.
    fs::write(workspace.path.join("notes"), "notes\n").unwrap();
    export_moving(&exporting, &selector, source.path()).await;

    let ended = exporting.end_moved_space(&selector).await.unwrap();
    assert!(ended.removed);
    assert!(!workspace.path.exists());
}

/// Whether the workspace `exporting` moves with every unsaved file it may
/// delete stays here once `change` changed it after its export, while the
/// move ends.
async fn changed_after_export_stays(change: impl FnOnce(&Path)) {
    let (source, exporting, source_repo, _origin) = exporter().await;
    let workspace = stopped_workspace(&exporting, &source_repo).await;
    let selector = SpaceSelector::id(&workspace.id);
    export_deleting(&exporting, &selector, source.path(), UnsavedWork::All).await;

    change(&workspace.path);
    assert!(exporting.end_moved_space(&selector).await.is_err());
    assert!(exporting.workspace_by_id(&workspace.id).await.is_ok());
    assert!(workspace.path.join("newer").exists());
    assert!(recorded(&exporting, &workspace.id).await.is_none());
}

#[tokio::test]
async fn a_workspace_with_a_new_unsaved_file_after_its_export_stays() {
    changed_after_export_stays(|path| fs::write(path.join("newer"), "newer\n").unwrap()).await;
}

#[tokio::test]
async fn a_workspace_with_a_new_commit_after_its_export_stays() {
    changed_after_export_stays(|path| {
        fs::write(path.join("newer"), "newer\n").unwrap();
        commit(path, "newer");
    })
    .await;
}

#[tokio::test]
async fn a_moved_workspace_whose_removal_hook_writes_a_file_stays() {
    let (source, exporting, source_repo, _origin) = exporter().await;
    let workspace = stopped_workspace(&exporting, &source_repo).await;
    let selector = SpaceSelector::id(&workspace.id);
    // Its package holds unsaved files, which the end may then delete.
    fs::write(workspace.path.join("notes"), "notes\n").unwrap();
    export_moving(&exporting, &selector, source.path()).await;
    write_on_removal(&exporting, &source_repo, source.path()).await;

    let error = exporting.end_moved_space(&selector).await.unwrap_err();
    assert!(format!("{error:#}").contains("changed here after its export"));
    assert!(exporting.workspace_by_id(&workspace.id).await.is_ok());
    assert!(workspace.path.join("hooked").exists());
}

#[tokio::test]
async fn a_carried_workspace_whose_removal_hook_writes_a_file_keeps_the_move_here() {
    let (source, exporting, source_repo, _origin) = exporter().await;
    let workspace = stopped_workspace(&exporting, &source_repo).await;
    let selector = SpaceSelector::id(&workspace.id);
    let child = (exporting.create_child(&selector, "child".into(), None, Default::default()))
        .await
        .unwrap();
    let taking = UnsavedWork::taking(vec![child.id.clone()]);
    export_deleting(&exporting, &selector, source.path(), taking).await;
    write_on_removal(&exporting, &source_repo, source.path()).await;

    let error = exporting.end_moved_space(&selector).await.unwrap_err();
    assert!(format!("{error:#}").contains("changed here after its export"));
    for kept in [&workspace, &child] {
        assert!(exporting.workspace_by_id(&kept.id).await.is_ok());
    }
    assert!(child.path.join("hooked").exists());
}

#[tokio::test]
async fn a_moving_chat_stops_its_agent_and_stays_until_its_end() {
    let (root, manager, pid) = super::chat("plan-cache").await;
    let chat = manager.chats().await.unwrap().remove(0);
    crate::daemon::test_support::report_session(&manager, &chat.id, "s1").await;
    let selector = SpaceSelector::id(&chat.id);

    let (_, file) = export_moving(&manager, &selector, root.path()).await;
    assert_eq!(identity::capture(pid).unwrap(), None);
    assert_eq!(manager.chats().await.unwrap()[0].id, chat.id);
    let import = Import::read(&mut &file[..]).unwrap();
    assert!(import.manifest.space.record.chat().is_some());

    let ended = manager.end_moved_space(&selector).await.unwrap();
    assert!(ended.removed);
    assert!(manager.chats().await.unwrap().is_empty());
}

#[tokio::test]
async fn a_chat_without_a_session_moves_with_a_warning() {
    let (root, manager, pid) = super::chat("talk").await;
    let chat = manager.chats().await.unwrap().remove(0);
    let selector = SpaceSelector::id(&chat.id);

    let (exported, _) = export_moving(&manager, &selector, root.path()).await;
    let warned = (exported.warnings.iter()).any(|warning| warning.contains("talk"));
    assert!(warned, "{:?}", exported.warnings);
    assert_eq!(identity::capture(pid).unwrap(), None);
    manager.end_moved_space(&selector).await.unwrap();
    assert!(manager.chats().await.unwrap().is_empty());
}

#[tokio::test]
async fn a_moving_chat_stops_the_chats_below_it_and_takes_them_at_its_end() {
    let (root, manager, _pid) = super::chat("lead").await;
    let lead = manager.chats().await.unwrap().remove(0);
    crate::daemon::test_support::report_session(&manager, &lead.id, "s1").await;
    let repo = manager.repositories().await.unwrap().remove(0);
    let child = test_support::chat(&manager, "child", "chat", Some(&repo), Some(&lead.id)).await;
    let mut process = test_support::agent_process(&manager, &child.id).await;
    let selector = SpaceSelector::id(&lead.id);

    let confirmed = UnsavedWork::taking(vec![child.id.clone()]);
    let (_, file) = export_deleting(&manager, &selector, root.path(), confirmed).await;
    process.wait().unwrap();
    assert_eq!(manager.chats().await.unwrap().len(), 2);
    let import = Import::read(&mut &file[..]).unwrap();
    let carried: Vec<_> = (import.manifest.carried.iter())
        .map(|carried| &carried.space.record.space_id)
        .collect();
    assert_eq!(carried, [&child.id]);

    manager.end_moved_space(&selector).await.unwrap();
    assert!(manager.chats().await.unwrap().is_empty());
}

/// A workspace of `exporting` with a child, whose agent wrote a file
/// while it stopped, after the caller's check, moving with it.
async fn moving_with_child(
    source: &Path,
    exporting: &Manager,
    repo: &crate::model::Repository,
) -> (Workspace, Workspace, ExportedSpace) {
    let workspace = stopped_workspace(exporting, repo).await;
    let selector = SpaceSelector::id(&workspace.id);
    let child = (exporting.create_child(&selector, "child".into(), None, Default::default()))
        .await
        .unwrap();
    fs::write(child.path.join("notes"), "notes\n").unwrap();
    let taking = UnsavedWork::taking(vec![child.id.clone()]);
    let (exported, _) = export_deleting(exporting, &selector, source, taking).await;
    (workspace, child, exported)
}

#[tokio::test]
async fn a_moving_workspace_takes_the_workspace_below_it_at_its_end() {
    let (source, exporting, source_repo, _origin) = exporter().await;
    let (workspace, child, exported) =
        moving_with_child(source.path(), &exporting, &source_repo).await;
    assert!(child.path.exists());
    assert_eq!(exported_checkpoints(&exported).len(), 2);

    let selector = SpaceSelector::id(&workspace.id);
    let ended = exporting.end_moved_space(&selector).await.unwrap();
    assert!(ended.removed);
    assert!(!child.path.exists());
    let removed = exporting.removed_workspaces().await.unwrap();
    assert_eq!(removed.len(), 2);
}

#[tokio::test]
async fn a_child_that_changed_after_its_export_keeps_both_here() {
    let (source, exporting, source_repo, _origin) = exporter().await;
    let (workspace, child, _) = moving_with_child(source.path(), &exporting, &source_repo).await;
    fs::write(child.path.join("newer"), "newer\n").unwrap();

    let selector = SpaceSelector::id(&workspace.id);
    assert!(exporting.end_moved_space(&selector).await.is_err());
    for kept in [&workspace, &child] {
        assert!(exporting.workspace_by_id(&kept.id).await.is_ok());
    }
}

/// The move of the space `id` that `manager` records.
async fn recorded(manager: &Manager, id: &SpaceId) -> Option<MoveRecord> {
    let id = id.clone();
    (manager.store)
        .read(move |db| super::super::records::read(db, &id))
        .await
        .unwrap()
}

#[tokio::test]
async fn an_import_that_records_nothing_says_so() {
    let (source, exporting, source_repo, origin) = exporter().await;
    let (_target, importing, _) = importer(&origin).await;
    let workspace = stopped_workspace(&exporting, &source_repo).await;
    let selector = SpaceSelector::id(&workspace.id);
    let (exported, _) = export_moving(&exporting, &selector, source.path()).await;
    let packaged = packaged_workspace(&exported.manifest);

    let import = SpaceImport {
        repository: Some(crate::repository::Selector::from("no-such-repository")),
        bundle: None,
        prerequisite: None,
        space: packaged.record.clone(),
        copy: false,
        session_files: Vec::new(),
        copies: Default::default(),
        recorded: Default::default(),
        claim: crate::daemon::restore::Claim::Plain,
    };
    let error = importing.import_space(import).await.unwrap_err();
    assert_eq!(Reason::of(&error), Some(Reason::NotImported));
    assert!(importing.removed_workspaces().await.unwrap().is_empty());
}

/// The finishing operations `manager` runs, by the space each finishes.
fn finishing(manager: &Manager) -> Vec<SpaceId> {
    (manager.operations.running().into_iter())
        .filter(|entry| entry.kind == OperationKind::FinishMove)
        .map(|entry| entry.workspace.clone())
        .collect()
}

#[tokio::test]
async fn a_move_whose_worker_went_finishes_in_a_worker_of_its_own() {
    let (source, exporting, source_repo, _origin) = exporter().await;
    let workspace = stopped_workspace(&exporting, &source_repo).await;
    let selector = SpaceSelector::id(&workspace.id);
    export_moving(&exporting, &selector, source.path()).await;

    // A worker that holds the move's lock finishes it itself.
    let held = MoveLock::try_take(&exporting.paths, &workspace.id)
        .unwrap()
        .unwrap();
    exporting.finish_interrupted_moves().await.unwrap();
    assert!(finishing(&exporting).is_empty());

    held.release();
    exporting.finish_interrupted_moves().await.unwrap();
    // A pass while the finishing worker runs starts no other.
    exporting.finish_interrupted_moves().await.unwrap();
    assert_eq!(finishing(&exporting), std::slice::from_ref(&workspace.id));
}

#[tokio::test]
async fn a_finished_move_tells_the_user_how_each_space_ended() {
    let (_root, manager) = test_support::manager().await;
    let report = MoveReport {
        moved: Vec::new(),
        failed: vec![MoveFailure {
            id: "talk".into(),
            name: "talk".into(),
            error: "mini may already have it".into(),
        }],
    };
    let ended = Ended::Succeeded(Box::new(Outcome::Moved(report)));
    manager.tell_finished_move(&ended).await;

    let alerts = manager.notifications.list(true, 10).await.unwrap();
    let [alert] = alerts.as_slice() else {
        panic!("expected one alert: {alerts:?}");
    };
    assert_eq!(alert.kind, AlertKind::MoveFinished);
    assert!(alert.message.contains("talk"), "{}", alert.message);
    assert!(alert.message.contains("mini may already have it"));
}
