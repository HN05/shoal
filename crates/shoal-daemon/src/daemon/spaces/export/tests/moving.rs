use shoal_ids::execution_ids::ExecutionId;
use std::fs;

use super::transfer::{exporter, import, importer, packaged_workspace};
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
    transfer::{Carrying, ExportedSpace, Import, ManifestExt, SpaceImport},
};
use shoal_ids::checkpoint_numbers::CheckpointNumber;
use shoal_ids::space_ids::SpaceId;
use shoal_protocol::space_names::SpaceSelector;
use std::collections::BTreeMap;
use std::path::Path;

/// Export `space` for a move into a new file in `directory` and read it
/// back.
async fn export_moving(
    manager: &Manager,
    space: &SpaceSelector,
    directory: &std::path::Path,
) -> (ExportedSpace, Vec<u8>) {
    let file = directory.join("moving.shoal");
    let exported = manager
        .export_moving_space(space, file.clone(), &Carrying::Subtree)
        .await
        .unwrap();
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
    assert!(exported.removal.is_none());
    assert!(workspace.path.join("work").exists());
    let packaged = packaged_workspace(&exported.manifest);
    assert_eq!(packaged.record.agents.len(), 1);
    let restored = import(&importing, &target_repo, &file).await;
    assert_eq!(
        fs::read_to_string(restored.opened.workspace.path.join("work")).unwrap(),
        "work\n"
    );

    let checkpoint = packaged.record.kept_workspace().unwrap().checkpoint.number;
    let ended = exporting
        .end_moved_space(
            &selector,
            UnsavedWork::default(),
            &BTreeMap::from([(workspace.id.clone(), checkpoint)]),
            &Carrying::Subtree,
        )
        .await
        .unwrap();
    assert!(ended.removed);
    assert!(!workspace.path.exists());
    let removed = exporting.removed_workspaces().await.unwrap();
    assert_eq!(removed[0].space_id, workspace.id);
    assert_eq!(removed[0].agents.len(), 1);
}

#[tokio::test]
async fn files_written_before_the_export_end_with_the_workspace_the_caller_saw_clean() {
    let (source, exporting, source_repo, _origin) = exporter().await;
    let workspace = stopped_workspace(&exporting, &source_repo).await;
    let selector = SpaceSelector::id(&workspace.id);
    // As an agent writes when it stops, after the caller's check.
    fs::write(workspace.path.join("notes"), "notes\n").unwrap();
    let (exported, _) = export_moving(&exporting, &selector, source.path()).await;
    let checkpoint = packaged_workspace(&exported.manifest)
        .record
        .kept_workspace()
        .unwrap()
        .checkpoint
        .number;

    let ended = exporting
        .end_moved_space(
            &selector,
            UnsavedWork::default(),
            &BTreeMap::from([(workspace.id.clone(), checkpoint)]),
            &Carrying::Subtree,
        )
        .await
        .unwrap();
    assert!(ended.removed);
    assert!(!workspace.path.exists());
}

#[tokio::test]
async fn a_workspace_that_changed_after_its_export_stays() {
    let (source, exporting, source_repo, _origin) = exporter().await;
    let workspace = stopped_workspace(&exporting, &source_repo).await;
    let selector = SpaceSelector::id(&workspace.id);
    let (exported, _) = export_moving(&exporting, &selector, source.path()).await;
    let checkpoint = packaged_workspace(&exported.manifest)
        .record
        .kept_workspace()
        .unwrap()
        .checkpoint
        .number;
    let exported = BTreeMap::from([(workspace.id.clone(), checkpoint)]);
    let end =
        || exporting.end_moved_space(&selector, UnsavedWork::All, &exported, &Carrying::Subtree);

    // A new unsaved file keeps it, and so does a commit made here.
    fs::write(workspace.path.join("newer"), "newer\n").unwrap();
    assert!(end().await.is_err());
    commit(&workspace.path, "newer");
    assert!(end().await.is_err());
    assert!(exporting.workspace_by_id(&workspace.id).await.is_ok());
    assert!(workspace.path.join("newer").exists());
}

#[tokio::test]
async fn a_moved_workspace_whose_removal_hook_writes_a_file_stays() {
    let (source, exporting, source_repo, _origin) = exporter().await;
    let workspace = stopped_workspace(&exporting, &source_repo).await;
    let selector = SpaceSelector::id(&workspace.id);
    // Its package holds unsaved files, which the end may then delete.
    fs::write(workspace.path.join("notes"), "notes\n").unwrap();
    let (exported, _) = export_moving(&exporting, &selector, source.path()).await;
    write_on_removal(&exporting, &source_repo, source.path()).await;

    let checkpoints = exported_checkpoints(&exported);
    let end = exporting.end_moved_space(
        &selector,
        UnsavedWork::default(),
        &checkpoints,
        &Carrying::Subtree,
    );
    let error = end.await.unwrap_err();
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
    let (exported, _) = export_moving(&exporting, &selector, source.path()).await;
    write_on_removal(&exporting, &source_repo, source.path()).await;

    let checkpoints = exported_checkpoints(&exported);
    let taking = UnsavedWork::taking(vec![child.id.clone()]);
    let end = exporting.end_moved_space(&selector, taking, &checkpoints, &Carrying::Subtree);
    let error = end.await.unwrap_err();
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

    let ended = manager
        .end_moved_space(
            &selector,
            UnsavedWork::default(),
            &BTreeMap::new(),
            &Carrying::Subtree,
        )
        .await
        .unwrap();
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
    let nothing = BTreeMap::new();
    (manager.end_moved_space(
        &selector,
        UnsavedWork::default(),
        &nothing,
        &Carrying::Subtree,
    ))
    .await
    .unwrap();
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

    let (_, file) = export_moving(&manager, &selector, root.path()).await;
    process.wait().unwrap();
    assert_eq!(manager.chats().await.unwrap().len(), 2);
    let import = Import::read(&mut &file[..]).unwrap();
    let carried: Vec<_> = (import.manifest.carried.iter())
        .map(|carried| &carried.space.record.space_id)
        .collect();
    assert_eq!(carried, [&child.id]);

    let confirmed = UnsavedWork::taking(vec![child.id.clone()]);
    let nothing = BTreeMap::new();
    (manager.end_moved_space(&selector, confirmed, &nothing, &Carrying::Subtree))
        .await
        .unwrap();
    assert!(manager.chats().await.unwrap().is_empty());
}

#[tokio::test]
async fn a_moving_workspace_takes_the_workspace_below_it_at_its_end() {
    let (source, exporting, source_repo, _origin) = exporter().await;
    let workspace = stopped_workspace(&exporting, &source_repo).await;
    let selector = SpaceSelector::id(&workspace.id);
    let child = (exporting.create_child(&selector, "child".into(), None, Default::default()))
        .await
        .unwrap();
    // As its agent writes while it stops, after the caller's check.
    fs::write(child.path.join("notes"), "notes\n").unwrap();

    let (exported, _) = export_moving(&exporting, &selector, source.path()).await;
    assert!(child.path.exists());
    let checkpoints = exported_checkpoints(&exported);
    assert_eq!(checkpoints.len(), 2);
    // A child that changed after its export keeps both here.
    fs::write(child.path.join("newer"), "newer\n").unwrap();
    let taking = UnsavedWork::taking(vec![child.id.clone()]);
    let end =
        |unsaved| exporting.end_moved_space(&selector, unsaved, &checkpoints, &Carrying::Subtree);
    assert!(end(taking.clone()).await.is_err());
    fs::remove_file(child.path.join("newer")).unwrap();

    let ended = end(taking).await.unwrap();
    assert!(ended.removed);
    assert!(!child.path.exists());
    let removed = exporting.removed_workspaces().await.unwrap();
    assert_eq!(removed.len(), 2);
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
        copies: BTreeMap::new(),
        carried: Vec::new(),
    };
    let error = importing.import_space(import).await.unwrap_err();
    assert_eq!(Reason::of(&error), Some(Reason::NotImported));
    assert!(importing.removed_workspaces().await.unwrap().is_empty());
}
