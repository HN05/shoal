use std::{collections::BTreeMap, fs};

use super::transfer::{exporter, importer};
use crate::{
    daemon::{
        test_support::{self, commit},
        workspace::Manager,
    },
    model::{Repository, RestoredSpace, Workspace},
    removal::{InspectionPolicy, UnsavedWork},
    transfer::{Carrying, Import, ManifestExt, PackagedSpaceExt, SpaceImport},
};
use shoal_ids::space_ids::SpaceId;
use shoal_protocol::space_names::SpaceSelector;

/// A workspace `worker` of `repo` with a child workspace `child` on a
/// branch of its own built on `worker`'s, with a commit and an unsaved
/// file, and a chat `notes` below the child.
async fn tree(manager: &std::sync::Arc<Manager>, repo: &Repository) -> [SpaceId; 3] {
    let worker = manager
        .create_workspace(&repo.id, "worker".into(), None, None, None)
        .await
        .unwrap();
    let selector = SpaceSelector::id(&worker.id);
    let base = Some("worker".to_owned());
    let child = (manager.split_workspace(&selector, "child".into(), base, Default::default()))
        .await
        .unwrap();
    fs::write(child.path.join("work"), "work\n").unwrap();
    commit(&child.path, "work");
    fs::write(child.path.join("scratch"), "notes\n").unwrap();
    let notes = test_support::chat(manager, "notes", "chat", Some(repo), Some(&child.id)).await;
    [worker.id, child.id, notes.id]
}

/// Import the package `bytes` into `repo` as `shoal move import` does: its
/// space, then each space it carries, a copy below the copies made before
/// it.
async fn import_package(manager: &Manager, repo: &Repository, bytes: &[u8]) -> Vec<SpaceId> {
    let import = Import::read(&mut &bytes[..]).unwrap();
    let manifest = &import.manifest;
    let own = std::iter::once((&manifest.space, manifest.sessions_on(None, None)));
    let carried = (manifest.carried.iter()).map(|carried| {
        (
            &carried.space,
            manifest.carried_sessions_on(carried, None, None),
        )
    });
    let mut copies = BTreeMap::new();
    let mut opened = Vec::new();
    for (packaged, sessions) in own.chain(carried) {
        let id = &packaged.record.space_id;
        let space = SpaceImport {
            repository: Some((&repo.id).into()),
            bundle: (import.bundles.get(id)).map(|bundle| bundle.path().to_owned()),
            prerequisite: packaged.prerequisite.clone(),
            space: packaged.record_with(sessions),
            copy: manifest.copy.is_some(),
            session_files: Vec::new(),
            copies: copies.clone(),
            carried: Vec::new(),
        };
        let restored = manager.import_space(space).await.unwrap();
        let restored = match restored {
            RestoredSpace::Workspace(restored) => restored.opened.workspace.id,
            RestoredSpace::Chat(restored) => restored.chat.id,
        };
        copies.insert(id.clone(), restored.clone());
        opened.push(restored);
    }
    opened
}

/// Export the space `id` of `manager` with the spaces below it and read
/// the package back.
async fn export_tree(manager: &Manager, id: &SpaceId, directory: &std::path::Path) -> Vec<u8> {
    let file = directory.join("tree.shoal");
    manager
        .export_space(
            &SpaceSelector::id(id),
            UnsavedWork::All,
            InspectionPolicy::GitOnly,
            file.clone(),
            &Carrying::Subtree,
        )
        .await
        .unwrap();
    fs::read(file).unwrap()
}

/// The workspace `id` of `manager`, with the ID of the space above it.
async fn placed(manager: &Manager, id: &SpaceId) -> (Workspace, Option<SpaceId>) {
    let workspace = manager.workspace_by_id(id).await.unwrap();
    let parent = workspace.parent.as_ref().map(|parent| parent.id.clone());
    (workspace, parent)
}

#[tokio::test]
async fn a_workspace_moves_with_its_subtree_which_opens_below_it_again() {
    let (source, exporting, source_repo, origin) = exporter().await;
    let (_target, importing, target_repo) = importer(&origin).await;
    let [worker, child, notes] = tree(&exporting, &source_repo).await;

    let bytes = export_tree(&exporting, &worker, source.path()).await;
    assert!(exporting.present_spaces().await.unwrap().is_empty());
    let import = Import::read(&mut &bytes[..]).unwrap();
    let carried: Vec<&SpaceId> = (import.manifest.carried.iter())
        .map(|carried| &carried.space.record.space_id)
        .collect();
    assert_eq!(carried, [&child, &notes]);
    assert!(import.bundles.contains_key(&child));

    let opened = import_package(&importing, &target_repo, &bytes).await;
    assert_eq!(opened, [worker.clone(), child.clone(), notes.clone()]);
    let (below, parent) = placed(&importing, &child).await;
    assert_eq!(parent.as_ref(), Some(&worker));
    assert_eq!(below.base_ref.as_deref(), Some("refs/heads/worker"));
    let base = below.part.base_workspace.as_ref().map(|base| &base.id);
    assert_eq!(base, Some(&worker));
    assert_eq!(
        fs::read_to_string(below.path.join("work")).unwrap(),
        "work\n"
    );
    assert_eq!(
        fs::read_to_string(below.path.join("scratch")).unwrap(),
        "notes\n"
    );
    let chat = importing.chat_by_id(&notes).await.unwrap().unwrap();
    assert_eq!(chat.parent.map(|parent| parent.id), Some(child));
}

#[tokio::test]
async fn a_removed_workspace_exports_with_the_spaces_its_removal_took() {
    let (source, exporting, source_repo, origin) = exporter().await;
    let (_target, importing, target_repo) = importer(&origin).await;
    let [worker, child, notes] = tree(&exporting, &source_repo).await;
    (exporting.remove_selected(
        &SpaceSelector::id(&worker),
        UnsavedWork::All,
        InspectionPolicy::GitOnly,
    ))
    .await
    .unwrap();

    let bytes = export_tree(&exporting, &worker, source.path()).await;
    // The records stay for a restore here.
    assert_eq!(exporting.removed_spaces().await.unwrap().len(), 3);

    let opened = import_package(&importing, &target_repo, &bytes).await;
    assert_eq!(opened, [worker.clone(), child.clone(), notes]);
    assert_eq!(placed(&importing, &child).await.1, Some(worker));
}

#[tokio::test]
async fn a_copied_subtree_opens_as_copies_below_the_copies() {
    let (source, manager, repo, _origin) = exporter().await;
    let [worker, child, notes] = tree(&manager, &repo).await;
    let file = source.path().join("copy.shoal");
    let selector = SpaceSelector::id(&worker);

    (manager.copy_space(&selector, None, file.clone(), &Carrying::Subtree))
        .await
        .unwrap();
    let opened = import_package(&manager, &repo, &fs::read(file).unwrap()).await;

    let [worker_copy, child_copy, notes_copy] = opened.try_into().unwrap();
    for (original, copy) in [
        (&worker, &worker_copy),
        (&child, &child_copy),
        (&notes, &notes_copy),
    ] {
        assert_ne!(original, copy);
        assert!(manager.find_space(original).await.unwrap().is_some());
    }
    let (copy, parent) = placed(&manager, &child_copy).await;
    assert_eq!(parent, Some(worker_copy.clone()));
    let (worker_copy, _) = placed(&manager, &worker_copy).await;
    let branch = worker_copy.branch().unwrap();
    assert_ne!(branch.as_str(), "worker");
    let base = format!("refs/heads/{branch}");
    assert_eq!(copy.base_ref.as_deref(), Some(base.as_str()));
    assert_eq!(
        fs::read_to_string(copy.path.join("scratch")).unwrap(),
        "notes\n"
    );
    let chat = manager.chat_by_id(&notes_copy).await.unwrap().unwrap();
    assert_eq!(chat.parent.map(|parent| parent.id), Some(child_copy));
}

#[tokio::test]
async fn a_carried_space_that_cannot_go_refuses_before_anything_ends() {
    let (source, manager, repo, _origin) = exporter().await;
    let worker = manager
        .create_workspace(&repo.id, "worker".into(), None, None, None)
        .await
        .unwrap();
    let options = crate::protocol::CreationOptions {
        disposable: true,
        ..Default::default()
    };
    let scratch = manager
        .split_workspace(
            &SpaceSelector::id(&worker.id),
            "scratch".into(),
            None,
            options,
        )
        .await
        .unwrap();
    let file = source.path().join("worker.shoal");

    let refused = manager
        .export_space(
            &SpaceSelector::id(&worker.id),
            UnsavedWork::All,
            InspectionPolicy::GitOnly,
            file.clone(),
            &Carrying::Subtree,
        )
        .await;

    let error = format!("{:#}", refused.unwrap_err());
    assert!(error.contains("scratch"), "{error}");
    assert!(!file.exists());
    for kept in [&worker, &scratch] {
        assert!(manager.find_workspace(&kept.id).await.unwrap().is_some());
    }
}

#[tokio::test]
async fn an_import_whose_carried_space_is_here_records_nothing() {
    let (source, exporting, source_repo, origin) = exporter().await;
    let (_target, importing, target_repo) = importer(&origin).await;
    let [worker, child, _notes] = tree(&exporting, &source_repo).await;
    let bytes = export_tree(&exporting, &worker, source.path()).await;
    let import = Import::read(&mut &bytes[..]).unwrap();
    let here = (importing.create_workspace(&target_repo.id, "here".into(), None, None, None))
        .await
        .unwrap();
    let carried = |space: SpaceId| crate::transfer::ImportTarget {
        repository: Some((&target_repo.id).into()),
        space,
    };
    let import = SpaceImport {
        repository: Some((&target_repo.id).into()),
        bundle: None,
        prerequisite: import.manifest.space.prerequisite.clone(),
        space: import.manifest.space.record.clone(),
        copy: false,
        session_files: Vec::new(),
        copies: BTreeMap::new(),
        carried: vec![carried(child), carried(here.id)],
    };

    let refused = importing.import_space(import).await.unwrap_err();

    let reason = crate::refusal::Reason::of(&refused);
    assert_eq!(reason, Some(crate::refusal::Reason::NotImported));
    assert!(importing.removed_spaces().await.unwrap().is_empty());
    assert!(importing.find_space(&worker).await.unwrap().is_none());
}
