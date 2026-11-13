use crate::client::execution_ids::ExecutionId;
use std::fs;

use super::transfer::{exporter, import, importer, packaged_workspace};
use crate::client::space_names::SpaceSelector;
use crate::{
    daemon::{test_support::commit, workspace::Manager},
    execution::recovery,
    model::Workspace,
    process::identity,
    removal::UnsavedWork,
    transfer::{ExportedSpace, Import, Space},
};

/// Export `space` for a move into a new file in `directory` and read it
/// back.
async fn export_moving(
    manager: &Manager,
    space: &SpaceSelector,
    directory: &std::path::Path,
) -> (ExportedSpace, Vec<u8>) {
    let file = directory.join("moving.shoal");
    let exported = manager
        .export_moving_space(space, file.clone())
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
        &crate::execution::TrackedAgent::named("claude"),
        Some("overload"),
        None,
    )
    .unwrap();
    workspace
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
    assert_eq!(packaged.workspace.agents.len(), 1);
    let restored = import(&importing, &target_repo, &file).await;
    assert_eq!(
        fs::read_to_string(restored.opened.workspace.path.join("work")).unwrap(),
        "work\n"
    );

    let checkpoint = packaged.workspace.checkpoint.number;
    let ended = exporting
        .end_moved_space(&selector, UnsavedWork::default(), Some(checkpoint))
        .await
        .unwrap();
    assert!(ended.is_some());
    assert!(!workspace.path.exists());
    let removed = exporting.removed_workspaces().await.unwrap();
    assert_eq!(removed[0].workspace_id, workspace.id);
    assert_eq!(removed[0].agents.len(), 1);
}

#[tokio::test]
async fn a_workspace_that_changed_after_its_export_stays() {
    let (source, exporting, source_repo, _origin) = exporter().await;
    let workspace = stopped_workspace(&exporting, &source_repo).await;
    let selector = SpaceSelector::id(&workspace.id);
    let (exported, _) = export_moving(&exporting, &selector, source.path()).await;
    let checkpoint = packaged_workspace(&exported.manifest)
        .workspace
        .checkpoint
        .number;
    let end = || exporting.end_moved_space(&selector, UnsavedWork::All, Some(checkpoint));

    // A new unsaved file keeps it, and so does a commit made here.
    fs::write(workspace.path.join("newer"), "newer\n").unwrap();
    assert!(end().await.is_err());
    commit(&workspace.path, "newer");
    assert!(end().await.is_err());
    assert!(exporting.workspace_by_id(&workspace.id).await.is_ok());
    assert!(workspace.path.join("newer").exists());
}

#[tokio::test]
async fn a_moving_chat_stops_its_agent_and_stays_until_its_end() {
    let (root, manager, pid) = super::chat("plan-cache").await;
    let chat = manager.chats().await.unwrap().remove(0);
    manager
        .record_chat_session(&chat.id, "s1".into())
        .await
        .unwrap();
    let selector = SpaceSelector::id(&chat.id);

    let (_, file) = export_moving(&manager, &selector, root.path()).await;
    assert_eq!(identity::capture(pid).unwrap(), None);
    assert_eq!(manager.chats().await.unwrap()[0].id, chat.id);
    let import = Import::read(&mut &file[..]).unwrap();
    assert!(matches!(import.manifest.space, Space::Chat(_)));

    let ended = manager
        .end_moved_space(&selector, UnsavedWork::default(), None)
        .await
        .unwrap();
    assert!(ended.is_none());
    assert!(manager.chats().await.unwrap().is_empty());
}
