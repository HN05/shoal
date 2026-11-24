use std::fs;

use super::Import;
use super::transfer::{exporter, importer};
use crate::{
    daemon::{
        manager::Manager,
        test_support::{self, commit, import_file},
    },
    model::{Repository, Workspace},
    removal::{InspectionPolicy, UnsavedWork},
    transfer::{CarriedRecord, Carrying, MoveDestination},
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
    let child = (manager.spaces.create_child(
        &**manager,
        &selector,
        "child".into(),
        base,
        Default::default(),
    ))
    .await
    .unwrap();
    fs::write(child.path.join("work"), "work\n").unwrap();
    commit(&child.path, "work");
    fs::write(child.path.join("scratch"), "notes\n").unwrap();
    let notes = test_support::chat(manager, "notes", "chat", Some(repo), Some(&child.id)).await;
    [worker.id, child.id, notes.id]
}

/// Import the package `bytes` into `repo` as `shoal restore --from` does:
/// the daemon opens its space, then the caller opens each space it carries,
/// in order; the IDs they opened under.
async fn import_package(manager: &Manager, repo: &Repository, bytes: &[u8]) -> Vec<SpaceId> {
    let imported = import_file(manager, Some(repo), bytes).await.unwrap();
    let mut opened = vec![imported.restored.id().clone()];
    for carried in imported.carried {
        let CarriedRecord::Recorded { space, .. } = carried else {
            panic!("a carried space was not recorded: {carried:?}");
        };
        let selector = SpaceSelector::id(&space);
        manager
            .spaces
            .restore_space(manager, &selector, None)
            .await
            .unwrap();
        opened.push(space);
    }
    opened
}

/// Export the space `id` of `manager` with the spaces below it and read
/// the package back.
async fn export_tree(manager: &Manager, id: &SpaceId, directory: &std::path::Path) -> Vec<u8> {
    let file = directory.join("tree.shoal");
    let (selector, all) = (SpaceSelector::id(id), UnsavedWork::All);
    let moving = crate::daemon::test_support::move_to_file(
        manager,
        &selector,
        all,
        file.clone(),
        &Carrying::Subtree,
    );
    moving.await.unwrap();
    fs::read(file).unwrap()
}

/// The workspace `id` of `manager`, with the ID of the space above it.
async fn placed(manager: &Manager, id: &SpaceId) -> (Workspace, Option<SpaceId>) {
    let workspace = manager.spaces.workspace_by_id(id).await.unwrap();
    let parent = workspace.parent.as_ref().map(|parent| parent.id.clone());
    (workspace, parent)
}

#[tokio::test]
async fn a_workspace_moves_with_its_subtree_which_opens_below_it_again() {
    let (source, exporting, source_repo, origin) = exporter().await;
    let (_target, importing, target_repo) = importer(&origin).await;
    let [worker, child, notes] = tree(&exporting, &source_repo).await;

    let bytes = export_tree(&exporting, &worker, source.path()).await;
    assert!(
        exporting
            .spaces
            .present_spaces(&*exporting)
            .await
            .unwrap()
            .is_empty()
    );
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
    assert_eq!(below.checkout.base_ref(), Some("refs/heads/worker"));
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
    let chat = importing.spaces.chat_by_id(&notes).await.unwrap().unwrap();
    assert_eq!(chat.parent.map(|parent| parent.id), Some(child));
}

#[tokio::test]
async fn a_removed_workspace_exports_with_the_spaces_its_removal_took() {
    let (source, exporting, source_repo, origin) = exporter().await;
    let (_target, importing, target_repo) = importer(&origin).await;
    let [worker, child, notes] = tree(&exporting, &source_repo).await;
    (exporting.spaces.remove_selected(
        &*exporting,
        &SpaceSelector::id(&worker),
        UnsavedWork::All,
        InspectionPolicy::GitOnly,
    ))
    .await
    .unwrap();

    let file = source.path().join("tree.shoal");
    let selector = SpaceSelector::id(&worker);
    (exporting.spaces.export_removed_space(
        &*exporting,
        &selector,
        file.clone(),
        &Carrying::Subtree,
    ))
    .await
    .unwrap();
    let bytes = fs::read(file).unwrap();
    // The records stay for a restore here.
    assert_eq!(exporting.spaces.removed_spaces().await.unwrap().len(), 3);

    let opened = import_package(&importing, &target_repo, &bytes).await;
    assert_eq!(opened, [worker.clone(), child.clone(), notes]);
    assert_eq!(placed(&importing, &child).await.1, Some(worker));
}

#[tokio::test]
async fn an_import_whose_carried_space_is_here_records_nothing() {
    let (source, exporting, source_repo, origin) = exporter().await;
    let (_target, importing, target_repo) = importer(&origin).await;
    let [worker, child, _notes] = tree(&exporting, &source_repo).await;
    // The child arrives first on its own, as a move that left it running.
    let file = source.path().join("child.shoal");
    let selector = SpaceSelector::id(&child);
    let packing = crate::daemon::spaces::MovePacking {
        unsaved: UnsavedWork::default(),
        destination: MoveDestination::File { path: file.clone() },
        carrying: Carrying::Subtree,
        again: false,
    };
    (exporting
        .spaces
        .export_moving_space(&*exporting, &selector, packing))
    .await
    .unwrap();
    import_file(&importing, Some(&target_repo), &fs::read(file).unwrap())
        .await
        .unwrap();
    let bytes = export_tree(&exporting, &worker, source.path()).await;

    let refused = (import_file(&importing, Some(&target_repo), &bytes).await).unwrap_err();

    let reason = crate::refusal::Reason::of(&refused);
    assert_eq!(reason, Some(crate::refusal::Reason::NotImported));
    let removed = importing.spaces.removed_spaces().await.unwrap();
    assert!(removed.iter().all(|removed| removed.space_id != worker));
    assert!(
        importing
            .spaces
            .find_space(&worker)
            .await
            .unwrap()
            .is_none()
    );
}

#[tokio::test]
async fn a_selection_leaves_the_other_spaces_below_one_level_up() {
    let (source, manager, repo, _origin) = exporter().await;
    let top = test_support::chat(&manager, "top", "chat", Some(&repo), None).await;
    let worker = test_support::working_below(&manager, &repo, "worker", &top.id, "worker").await;
    let below = SpaceSelector::id(&worker.id);
    let child = test_support::working_below(&manager, &repo, "child", &worker.id, "worker").await;
    let base = Some("worker".to_owned());
    let stacked = (manager.spaces.create_child(
        &*manager,
        &below,
        "stacked".into(),
        base,
        Default::default(),
    ))
    .await
    .unwrap();
    let picked =
        (manager
            .spaces
            .create_child(&*manager, &below, "picked".into(), None, Default::default()))
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
    let held = (manager.spaces.request_hold(
        &*manager,
        &SpaceSelector::id(&child.id),
        "review".into(),
        reason,
    ))
    .await
    .unwrap();
    let crate::daemon::allocation::Allocation::Approval(request) = held else {
        panic!("the hold waits for approval");
    };
    let decider = async || {
        let request = manager
            .coordination
            .approval_by_key(&request.key)
            .await
            .unwrap();
        request.unwrap().decider
    };
    assert_eq!(decider().await, Some(worker.id.clone()));
    let file = source.path().join("worker.shoal");

    let carrying = Carrying::Selected(vec![picked.id.clone()]);
    let all = UnsavedWork::All;
    (crate::daemon::test_support::move_to_file(&manager, &below, all, file.clone(), &carrying))
        .await
        .unwrap();

    let import = Import::read(&mut &fs::read(&file).unwrap()[..]).unwrap();
    let carried: Vec<&SpaceId> = (import.manifest.carried.iter())
        .map(|carried| &carried.space.record.space_id)
        .collect();
    assert_eq!(carried, [&picked.id]);
    for gone in [&worker.id, &picked.id] {
        assert!(manager.spaces.find_space(gone).await.unwrap().is_none());
    }
    assert_eq!(placed(&manager, &child.id).await.1.as_ref(), Some(&top.id));
    let (stacked, parent) = placed(&manager, &stacked.id).await;
    assert_eq!(parent.as_ref(), Some(&top.id));
    assert_eq!(stacked.checkout.base_ref(), Some("refs/heads/worker"));
    // Neither the removal nor a later sweep of kept branches deletes it.
    (manager
        .spaces
        .delete_kept_branches_kept_elsewhere(&*manager, &repo.id, &[]))
    .await
    .unwrap();
    test_support::git(&repo.path, &["rev-parse", "--verify", "refs/heads/worker"]);
    assert_eq!(decider().await, Some(top.id.clone()));
}
