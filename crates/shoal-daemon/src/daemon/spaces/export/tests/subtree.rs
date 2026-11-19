use std::{collections::BTreeMap, fs};

use super::transfer::{exporter, importer};
use crate::{
    daemon::{
        manager::Manager,
        test_support::{self, commit},
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
    let child = (manager.create_child(&selector, "child".into(), base, Default::default()))
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
    let reading = manager
        .start_chat_in_workspace(
            &SpaceSelector::id(&child),
            "reading".into(),
            "claude".into(),
            None,
        )
        .await
        .unwrap();
    let file = source.path().join("copy.shoal");
    let selector = SpaceSelector::id(&worker);

    (manager.copy_space(&selector, None, file.clone(), &Carrying::Subtree))
        .await
        .unwrap();
    let opened = import_package(&manager, &repo, &fs::read(file).unwrap()).await;

    let [worker_copy, child_copy, notes_copy, reading_copy] = opened.try_into().unwrap();
    let reading_copy = manager.chat_by_id(&reading_copy).await.unwrap().unwrap();
    assert_ne!(reading_copy.id, reading.id);
    let ran_in = reading_copy
        .workdir
        .workspace()
        .map(|workspace| &workspace.id);
    assert_eq!(ran_in, Some(&child_copy));
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
        .create_child(
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

#[tokio::test]
async fn a_selection_leaves_the_other_spaces_below_one_level_up() {
    let (source, manager, repo, _origin) = exporter().await;
    let top = test_support::chat(&manager, "top", "chat", Some(&repo), None).await;
    let worker = test_support::working_below(&manager, &repo, "worker", &top.id, "worker").await;
    let below = SpaceSelector::id(&worker.id);
    let child = test_support::working_below(&manager, &repo, "child", &worker.id, "worker").await;
    let base = Some("worker".to_owned());
    let stacked = (manager.create_child(&below, "stacked".into(), base, Default::default()))
        .await
        .unwrap();
    let picked = (manager.create_child(&below, "picked".into(), None, Default::default()))
        .await
        .unwrap();
    // Its link to the base workspace can go while it still builds on that
    // workspace's branch.
    let unlinked = stacked.id.clone();
    (manager.store)
        .run(move |db| {
            db.execute(
                "UPDATE workspaces SET base_workspace_id=NULL WHERE id=?1",
                [unlinked],
            )?;
            Ok(())
        })
        .await
        .unwrap();
    let reason = Some("review it".to_owned());
    let held = (manager.request_hold(&SpaceSelector::id(&child.id), "review".into(), reason))
        .await
        .unwrap();
    let crate::daemon::allocation::Allocation::Approval(request) = held else {
        panic!("the hold waits for approval");
    };
    let decider = async || {
        let request = manager.approval_by_key(&request.key).await.unwrap();
        request.unwrap().decider
    };
    assert_eq!(decider().await, Some(worker.id.clone()));
    let file = source.path().join("worker.shoal");

    (manager.export_space(
        &below,
        UnsavedWork::All,
        InspectionPolicy::GitOnly,
        file.clone(),
        &Carrying::Selected(vec![picked.id.clone()]),
    ))
    .await
    .unwrap();

    let import = Import::read(&mut &fs::read(&file).unwrap()[..]).unwrap();
    let carried: Vec<&SpaceId> = (import.manifest.carried.iter())
        .map(|carried| &carried.space.record.space_id)
        .collect();
    assert_eq!(carried, [&picked.id]);
    for gone in [&worker.id, &picked.id] {
        assert!(manager.find_space(gone).await.unwrap().is_none());
    }
    assert_eq!(placed(&manager, &child.id).await.1.as_ref(), Some(&top.id));
    let (stacked, parent) = placed(&manager, &stacked.id).await;
    assert_eq!(parent.as_ref(), Some(&top.id));
    assert_eq!(stacked.base_ref.as_deref(), Some("refs/heads/worker"));
    // Neither the removal nor a later sweep of kept branches deletes it.
    (manager.delete_kept_branches_kept_elsewhere(&repo.id, &[]))
        .await
        .unwrap();
    test_support::git(&repo.path, &["rev-parse", "--verify", "refs/heads/worker"]);
    assert_eq!(decider().await, Some(top.id.clone()));
}

#[tokio::test]
async fn a_selection_moves_nothing_while_a_removal_holds_the_space_above() {
    let (source, manager, repo, _origin) = exporter().await;
    let top = test_support::chat(&manager, "top", "chat", Some(&repo), None).await;
    let worker = test_support::working_below(&manager, &repo, "worker", &top.id, "worker").await;
    let below = SpaceSelector::id(&worker.id);
    let child = (manager.create_child(&below, "child".into(), None, Default::default()))
        .await
        .unwrap();
    let _removing = manager.space_removals.reserve(&top.id).unwrap();

    let refused = manager
        .export_space(
            &below,
            UnsavedWork::All,
            InspectionPolicy::GitOnly,
            source.path().join("worker.shoal"),
            &Carrying::Selected(Vec::new()),
        )
        .await;

    assert!(refused.is_err());
    assert!(manager.find_space(&worker.id).await.unwrap().is_some());
    assert_eq!(placed(&manager, &child.id).await.1, Some(worker.id));
}

#[tokio::test]
async fn a_copy_carries_a_disposable_workspace_below_it() {
    let (source, manager, repo, _origin) = exporter().await;
    let worker = manager
        .create_workspace(&repo.id, "worker".into(), None, None, None)
        .await
        .unwrap();
    let selector = SpaceSelector::id(&worker.id);
    let options = crate::protocol::CreationOptions {
        disposable: true,
        ..Default::default()
    };
    let scratch = (manager.create_child(&selector, "scratch".into(), None, options))
        .await
        .unwrap();
    let file = source.path().join("copy.shoal");

    (manager.copy_space(&selector, None, file.clone(), &Carrying::Subtree))
        .await
        .unwrap();

    let import = Import::read(&mut &fs::read(&file).unwrap()[..]).unwrap();
    let carried: Vec<&SpaceId> = (import.manifest.carried.iter())
        .map(|carried| &carried.space.record.space_id)
        .collect();
    assert_eq!(carried, [&scratch.id]);
    assert!(manager.find_space(&scratch.id).await.unwrap().is_some());
}
