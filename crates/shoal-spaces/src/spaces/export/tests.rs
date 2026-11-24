use std::{collections::BTreeMap, fs, path::Path};

use anyhow::Result;

use crate::execution::recovery::Copied;
use crate::notifications::AlertKind;
use crate::restore::SpaceImport;
use crate::stand_in::Fixture;
use crate::{
    model::{RemovedSpace, Repository, Space, Workspace},
    protocol::operations::{Ended, Outcome},
    removal::{InspectionPolicy, UnsavedWork},
    transfer::{
        Carrying, ExportedSpace, ImportedSessionFile, Manifest, ManifestExt, MoveDestination,
        MoveFailure, MoveReport, PackagedSpace, PackagedSpaceExt, Parts,
    },
};
use shoal_core::test_support::{commit, git};
use shoal_ids::space_ids::SpaceId;
use shoal_protocol::space_names::SpaceSelector;
use shoal_protocol::transfer::CarriedRecord;

/// A package read back as an import reads it, with the directory its parts
/// are in.
struct Import {
    manifest: Manifest,
    bundles: BTreeMap<SpaceId, tempfile::NamedTempFile>,
    _sessions: Vec<ImportedSessionFile>,
    _directory: tempfile::TempDir,
}

impl Import {
    fn read(input: &mut &[u8]) -> Result<Self> {
        let directory = tempfile::tempdir()?;
        let manifest = crate::transfer::read_manifest(input)?;
        let Parts { sessions, bundles } = Parts::read(input, &manifest, directory.path())?;
        Ok(Self {
            manifest,
            bundles,
            _sessions: sessions,
            _directory: directory,
        })
    }
}

/// A registered repository whose `origin` has its main branch.
async fn exporter() -> (Fixture, Repository) {
    let manager = Fixture::new().await;
    let repo = manager.repository("repo").await;
    let origin = manager.root.path().join("origin.git");
    let origin = origin.to_str().unwrap();
    git(
        manager.root.path(),
        &["init", "-q", "--bare", "-b", "main", origin],
    );
    git(&repo.path, &["remote", "add", "origin", origin]);
    git(&repo.path, &["push", "-q", "-u", "origin", "main"]);
    git(&repo.path, &["remote", "set-head", "origin", "main"]);
    (manager, repo)
}

/// The chat `name` of a new repository, whose agent runs.
async fn running_chat(manager: &Fixture, name: &str) -> crate::model::Chat {
    let repo = manager.repository("repo").await;
    let chat = manager.chat(name, "chat", Some(&repo), None).await;
    manager.run_agent(&chat.id).await;
    chat
}

/// The chat `name` in `mode` of a new repository, removed once its agent
/// reported `session`, when it did.
async fn removed_chat(
    manager: &Fixture,
    name: &str,
    mode: &str,
    session: Option<&str>,
) -> crate::model::Chat {
    let repo = manager.repository("repo").await;
    let chat = manager.chat(name, mode, Some(&repo), None).await;
    manager.run_agent(&chat.id).await;
    if let Some(session) = session {
        manager.report_session(&chat.id, session);
    }
    manager
        .spaces
        .remove_selected(
            &**manager,
            &SpaceSelector::id(&chat.id),
            UnsavedWork::default(),
            InspectionPolicy::GitOnly,
        )
        .await
        .unwrap();
    chat
}

/// The workspace `name` of `repository` below `parent`, whose agent works.
async fn working_below(
    manager: &Fixture,
    repository: &Repository,
    name: &str,
    parent: &SpaceId,
) -> Workspace {
    let workspace = manager
        .spaces
        .create_workspace_below(
            &**manager,
            repository.id.clone().into(),
            name.into(),
            None,
            Default::default(),
            Some(parent.clone()),
        )
        .await
        .unwrap();
    manager.run_agent(&workspace.id).await;
    manager.report(&workspace.id, crate::state::AgentState::Working);
    workspace
}

/// What `shoal restore --from` sends to import the carried chat `chat` into
/// `repository`, or into none.
fn chat_import(repository: Option<&Repository>, chat: RemovedSpace) -> SpaceImport {
    SpaceImport {
        repository: repository.map(|repository| (&repository.id).into()),
        bundle: None,
        prerequisite: None,
        space: chat,
        copy: false,
        session_files: Vec::new(),
        copies: Default::default(),
        recorded: Default::default(),
        claim: crate::transfer::Claim::Plain,
    }
}

/// Move the space `selector` names to the new file `file`, letting its end
/// delete any unsaved work.
async fn export(manager: &Fixture, selector: &str, file: &Path) -> Result<ExportedSpace> {
    let selector = manager.named(selector).await;
    let (all, file) = (UnsavedWork::All, file.to_owned());
    let moving = manager.move_to_file(&selector, all, file, &Carrying::Subtree);
    Ok(moving.await?.0)
}

fn warns_of(exported: &ExportedSpace, name: &str) -> bool {
    (exported.warnings.iter()).any(|warning| warning.contains(name))
}

/// Export the removed space `id` to `file`.
async fn export_removed(manager: &Fixture, id: &SpaceId, file: &Path) -> Result<ExportedSpace> {
    manager
        .spaces
        .export_removed_space(
            &**manager,
            &SpaceSelector::id(id),
            file.to_owned(),
            &Carrying::Subtree,
        )
        .await
}

/// The space `id` here.
async fn space(manager: &Fixture, id: &SpaceId) -> Space {
    manager.spaces.find_space(id).await.unwrap().unwrap()
}

/// A workspace `worker` of `repo` with a child workspace `child` on a
/// branch of its own built on `worker`'s, with a commit and an unsaved
/// file, and a chat `notes` below the child.
async fn tree(manager: &Fixture, repo: &Repository) -> [SpaceId; 3] {
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
    let notes = manager
        .chat("notes", "chat", Some(repo), Some(&child.id))
        .await;
    [worker.id, child.id, notes.id]
}

/// Import the package `bytes` into `repo` as `shoal restore --from` does:
/// the daemon opens its space, then the caller opens each space it carries,
/// in order; the IDs they opened under.
async fn import_package(manager: &Fixture, repo: &Repository, bytes: &[u8]) -> Vec<SpaceId> {
    let imported = manager.import_file(Some(repo), bytes).await.unwrap();
    let mut opened = vec![imported.restored.id().clone()];
    for carried in imported.carried {
        let CarriedRecord::Recorded { space, .. } = carried else {
            panic!("a carried space was not recorded: {carried:?}");
        };
        let selector = SpaceSelector::id(&space);
        (manager.spaces.restore_space(&**manager, &selector, None))
            .await
            .unwrap();
        opened.push(space);
    }
    opened
}

/// The workspace `id` of `manager`, with the ID of the space above it.
async fn placed(manager: &Fixture, id: &SpaceId) -> (Workspace, Option<SpaceId>) {
    let workspace = manager.spaces.workspace_by_id(id).await.unwrap();
    let parent = workspace.parent.as_ref().map(|parent| parent.id.clone());
    (workspace, parent)
}

/// Import the workspace the package `file` holds into `repo`.
async fn import(
    manager: &Fixture,
    repo: &Repository,
    file: &[u8],
) -> crate::model::RestoredWorkspace {
    let import = Import::read(&mut &file[..]).unwrap();
    let packaged = packaged_workspace(&import.manifest);
    import_record(manager, repo, &import, packaged.record.clone()).await
}

/// Import the workspace the package `file` holds as `shoal restore --from`
/// does, which keeps the sessions on the exporting machine.
async fn import_here(
    manager: &Fixture,
    repo: &Repository,
    file: &[u8],
) -> crate::model::RestoredWorkspace {
    let import = Import::read(&mut &file[..]).unwrap();
    let sessions = import
        .manifest
        .sessions_on(crate::process::machine::id().as_deref(), None);
    let workspace = packaged_workspace(&import.manifest).record_with(sessions);
    import_record(manager, repo, &import, workspace).await
}

async fn import_record(
    manager: &Fixture,
    repo: &Repository,
    import: &Import,
    workspace: RemovedSpace,
) -> crate::model::RestoredWorkspace {
    let packaged = packaged_workspace(&import.manifest);
    let import = SpaceImport {
        repository: Some((&repo.id).into()),
        bundle: (import.bundles.get(&workspace.space_id)).map(|bundle| bundle.path().to_owned()),
        prerequisite: packaged.prerequisite.clone(),
        space: workspace,
        copy: import.manifest.copy.is_some(),
        session_files: Vec::new(),
        copies: Default::default(),
        recorded: Default::default(),
        claim: crate::transfer::Claim::Plain,
    };
    match manager
        .spaces
        .import_space(&**manager, import)
        .await
        .unwrap()
    {
        crate::model::RestoredSpace::Workspace(restored) => *restored,
        crate::model::RestoredSpace::Chat(_) => panic!("the import opened a chat"),
    }
}

/// The workspace `manifest` packages.
fn packaged_workspace(manifest: &Manifest) -> &PackagedSpace {
    assert!(
        manifest.space.record.workspace().is_some(),
        "a chat package: {manifest:?}"
    );
    &manifest.space
}

#[tokio::test]
async fn a_chat_whose_package_cannot_be_written_stays_here() {
    let manager = Fixture::new().await;
    let root = &manager.root;
    running_chat(&manager, "unwritten").await;
    let chat = manager.spaces.chats().await.unwrap().remove(0);
    manager.report_session(&chat.id, "s1");
    // A file the package would replace, which it never does.
    let file = root.path().join("unwritten.shoal");
    fs::write(&file, "kept").unwrap();

    assert!(export(&manager, "unwritten", &file).await.is_err());
    assert_eq!(manager.spaces.chats().await.unwrap()[0].id, chat.id);
    assert_eq!(fs::read_to_string(&file).unwrap(), "kept");
}

#[tokio::test]
async fn a_removed_chat_without_a_session_exports_with_a_warning() {
    let manager = Fixture::new().await;
    let root = &manager.root;
    let chat = removed_chat(&manager, "talk", "chat", None).await;
    let file = root.path().join("talk.shoal");

    let exported = export_removed(&manager, &chat.id, &file).await.unwrap();
    assert!(warns_of(&exported, "talk"), "{:?}", exported.warnings);
    assert!(file.exists());
    assert_eq!(manager.spaces.removed_spaces().await.unwrap().len(), 1);
}

#[tokio::test]
async fn a_chat_without_a_repository_moves_and_copies_for_root_dir() {
    let manager = Fixture::new().await;
    let root = &manager.root;
    let chat = manager.chat("lead", "orchestrate", None, None).await;
    manager.run_agent(&chat.id).await;
    manager.report_session(&chat.id, "s1");
    let selector = SpaceSelector::id(&chat.id);
    let copy = root.path().join("copy.shoal");
    let moved = root.path().join("lead.shoal");

    manager
        .spaces
        .copy_space(&*manager, &selector, None, copy.clone(), &Carrying::Subtree)
        .await
        .unwrap();
    let all = UnsavedWork::All;
    let moving = manager.move_to_file(&selector, all, moved.clone(), &Carrying::Subtree);
    let (_, removal) = moving.await.unwrap();
    assert!(removal.removed);
    assert!(manager.spaces.chats().await.unwrap().is_empty());
    for file in [&copy, &moved] {
        let import = Import::read(&mut &fs::read(file).unwrap()[..]).unwrap();
        let packaged = &import.manifest.space.record;
        assert!(packaged.chat().is_some());
        assert_eq!(packaged.repository_id, None);
        assert_eq!(import.manifest.remote_url, None);
    }
}

#[tokio::test]
async fn an_import_under_the_id_of_a_space_here_is_refused_before_recording_it() {
    let manager = Fixture::new().await;
    let root = &manager.root;
    let chat = removed_chat(&manager, "notes", "chat", Some("s1")).await;
    let file = root.path().join("notes.shoal");
    export_removed(&manager, &chat.id, &file).await.unwrap();
    // Its record stays, so it can open here again before the import.
    let selector = SpaceSelector::id(&chat.id);
    manager
        .spaces
        .restore_space(&*manager, &selector, None)
        .await
        .unwrap();
    let import = Import::read(&mut &fs::read(&file).unwrap()[..]).unwrap();
    let repository = (chat.repository_id.as_ref()).map(|id| id.clone().into());
    let space = SpaceImport {
        repository,
        ..chat_import(None, import.manifest.space.record)
    };

    let refused = manager
        .spaces
        .import_space(&*manager, space)
        .await
        .unwrap_err();

    assert_eq!(
        crate::refusal::Reason::of(&refused),
        Some(crate::refusal::Reason::NotImported)
    );
    assert!(manager.spaces.removed_spaces().await.unwrap().is_empty());
    assert!(manager.spaces.chat_by_id(&chat.id).await.unwrap().is_some());
}

#[tokio::test]
async fn a_retired_workspace_keeps_a_checkpoint_and_leaves_the_spaces_below_it() {
    let (manager, repo) = exporter().await;
    let fix = manager
        .create_workspace(repo.id.clone(), "fix".into(), None, None, None)
        .await
        .unwrap();
    let below = manager
        .chat("below", "chat", Some(&repo), Some(&fix.id))
        .await;
    let space = space(&manager, &fix.id).await;
    let opens = manager
        .spaces
        .retire_space(&*manager, &space)
        .await
        .unwrap();
    let removed = (manager.spaces.removed_spaces().await.unwrap().into_iter())
        .find(|removed| removed.space_id == fix.id)
        .unwrap();
    let number = removed.workspace().unwrap().checkpoint.number;
    // Its ID names the space on the machine that took it over, so a copy
    // of the work kept here names its checkpoint.
    let copy = format!("--copy {} --checkpoint {number}", fix.id);
    assert!(opens.contains(&copy), "{opens}");
    assert!(
        manager
            .spaces
            .find_space(&below.id)
            .await
            .unwrap()
            .is_some()
    );
}

#[tokio::test]
async fn a_finished_move_tells_the_user_how_each_space_ended() {
    let manager = Fixture::new().await;
    let report = MoveReport {
        moved: Vec::new(),
        failed: vec![MoveFailure {
            id: "talk".into(),
            name: "talk".into(),
            error: "mini may already have it".into(),
        }],
    };
    let ended = Ended::Succeeded(Box::new(Outcome::Moved(report)));
    manager.spaces.tell_finished_move(&*manager, &ended).await;

    let alerts = manager.spaces.notifications.list(true, 10).await.unwrap();
    let [alert] = alerts.as_slice() else {
        panic!("expected one alert: {alerts:?}");
    };
    assert_eq!(alert.kind, AlertKind::MoveFinished);
    assert!(alert.message.contains("talk"), "{}", alert.message);
    assert!(alert.message.contains("mini may already have it"));
}

#[tokio::test]
async fn a_copied_subtree_opens_as_copies_below_the_copies() {
    let (manager, repo) = exporter().await;
    let [worker, child, notes] = tree(&manager, &repo).await;
    let reading = manager
        .chat_in(&SpaceSelector::id(&child), "reading")
        .await
        .unwrap();
    let file = manager.root.path().join("copy.shoal");
    let selector = SpaceSelector::id(&worker);

    (manager
        .spaces
        .copy_space(&*manager, &selector, None, file.clone(), &Carrying::Subtree))
    .await
    .unwrap();
    let opened = import_package(&manager, &repo, &fs::read(file).unwrap()).await;

    let [worker_copy, child_copy, notes_copy, reading_copy] = opened.try_into().unwrap();
    let reading_copy = manager
        .spaces
        .chat_by_id(&reading_copy)
        .await
        .unwrap()
        .unwrap();
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
        assert!(manager.spaces.find_space(original).await.unwrap().is_some());
    }
    let (copy, parent) = placed(&manager, &child_copy).await;
    assert_eq!(parent, Some(worker_copy.clone()));
    let (worker_copy, _) = placed(&manager, &worker_copy).await;
    let branch = worker_copy.branch().unwrap();
    assert_ne!(branch.as_str(), "worker");
    let base = format!("refs/heads/{branch}");
    assert_eq!(copy.checkout.base_ref(), Some(base.as_str()));
    assert_eq!(
        fs::read_to_string(copy.path.join("scratch")).unwrap(),
        "notes\n"
    );
    let chat = manager
        .spaces
        .chat_by_id(&notes_copy)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(chat.parent.map(|parent| parent.id), Some(child_copy));
}

#[tokio::test]
async fn a_carried_space_that_cannot_go_refuses_before_anything_ends() {
    let (manager, repo) = exporter().await;
    let worker = manager
        .create_workspace(&repo.id, "worker".into(), None, None, None)
        .await
        .unwrap();
    let options = crate::protocol::CreationOptions {
        disposable: true,
        ..Default::default()
    };
    let scratch = manager
        .spaces
        .create_child(
            &*manager,
            &SpaceSelector::id(&worker.id),
            "scratch".into(),
            None,
            options,
        )
        .await
        .unwrap();
    let file = manager.root.path().join("worker.shoal");

    let (selector, all) = (SpaceSelector::id(&worker.id), UnsavedWork::All);
    let refused = manager
        .move_to_file(&selector, all, file.clone(), &Carrying::Subtree)
        .await;

    let error = format!("{:#}", refused.unwrap_err());
    assert!(error.contains("scratch"), "{error}");
    assert!(!file.exists());
    for kept in [&worker, &scratch] {
        assert!(
            manager
                .spaces
                .find_workspace(&kept.id)
                .await
                .unwrap()
                .is_some()
        );
    }
}

#[tokio::test]
async fn a_move_to_a_joined_machine_leaves_the_spaces_below_it_below_it() {
    let (manager, repo) = exporter().await;
    let top = manager.chat("top", "chat", Some(&repo), None).await;
    let worker = working_below(&manager, &repo, "worker", &top.id).await;
    let below = SpaceSelector::id(&worker.id);
    let child =
        (manager
            .spaces
            .create_child(&*manager, &below, "child".into(), None, Default::default()))
        .await
        .unwrap();
    let packing = crate::spaces::MovePacking {
        unsaved: UnsavedWork::All,
        destination: MoveDestination::Machine(crate::transfer::MachineDestination {
            machine: "desk".into(),
            resume: true,
        }),
        carrying: Carrying::Selected(Vec::new()),
        again: false,
    };

    manager
        .spaces
        .export_moving_space(&*manager, &below, packing)
        .await
        .unwrap();
    manager
        .spaces
        .end_moved_space(&*manager, &below)
        .await
        .unwrap();

    assert!(
        manager
            .spaces
            .find_space(&worker.id)
            .await
            .unwrap()
            .is_none()
    );
    // It stays below the worker, which runs on the other machine now.
    let id = child.id.clone();
    let parent = (manager.store)
        .read(move |db| crate::tree::parent_id(db, &id))
        .await
        .unwrap();
    assert_eq!(parent, Some(worker.id));
}

#[tokio::test]
async fn a_space_left_detached_links_again_once_no_move_takes_its_parent() {
    let (manager, repo) = exporter().await;
    let worker = (manager.create_workspace(&repo.id, "worker".into(), None, None, None))
        .await
        .unwrap();
    let below = SpaceSelector::id(&worker.id);
    let child =
        (manager
            .spaces
            .create_child(&*manager, &below, "child".into(), None, Default::default()))
        .await
        .unwrap();
    let parent = async || {
        let id = child.id.clone();
        (manager.store)
            .read(move |db| crate::tree::parent_id(db, &id))
            .await
            .unwrap()
    };
    // As a daemon that stopped during the moved space's removal left it.
    let moving = manager.spaces.space(&below).await.unwrap();
    manager.spaces.detach_behind(&moving, &[]).await.unwrap();
    assert_eq!(parent().await, None);

    manager
        .spaces
        .finish_interrupted_moves(&*manager)
        .await
        .unwrap();

    assert_eq!(parent().await, Some(worker.id));
}

#[tokio::test]
async fn a_selection_moves_nothing_while_a_removal_holds_the_space_above() {
    let (manager, repo) = exporter().await;
    let top = manager.chat("top", "chat", Some(&repo), None).await;
    let worker = working_below(&manager, &repo, "worker", &top.id).await;
    let below = SpaceSelector::id(&worker.id);
    let child =
        (manager
            .spaces
            .create_child(&*manager, &below, "child".into(), None, Default::default()))
        .await
        .unwrap();
    let _removing = manager.spaces.space_removals.reserve(&top.id).unwrap();

    let (file, carrying) = (
        manager.root.path().join("worker.shoal"),
        Carrying::Selected(Vec::new()),
    );
    let refused = manager
        .move_to_file(&below, UnsavedWork::All, file, &carrying)
        .await;

    assert!(refused.is_err());
    assert!(
        manager
            .spaces
            .find_space(&worker.id)
            .await
            .unwrap()
            .is_some()
    );
    assert_eq!(placed(&manager, &child.id).await.1, Some(worker.id));
}

#[tokio::test]
async fn a_copy_carries_a_disposable_workspace_below_it() {
    let (manager, repo) = exporter().await;
    let worker = manager
        .create_workspace(&repo.id, "worker".into(), None, None, None)
        .await
        .unwrap();
    let selector = SpaceSelector::id(&worker.id);
    let options = crate::protocol::CreationOptions {
        disposable: true,
        ..Default::default()
    };
    let scratch =
        (manager
            .spaces
            .create_child(&*manager, &selector, "scratch".into(), None, options))
        .await
        .unwrap();
    let file = manager.root.path().join("copy.shoal");

    (manager
        .spaces
        .copy_space(&*manager, &selector, None, file.clone(), &Carrying::Subtree))
    .await
    .unwrap();

    let import = Import::read(&mut &fs::read(&file).unwrap()[..]).unwrap();
    let carried: Vec<&SpaceId> = (import.manifest.carried.iter())
        .map(|carried| &carried.space.record.space_id)
        .collect();
    assert_eq!(carried, [&scratch.id]);
    assert!(
        manager
            .spaces
            .find_space(&scratch.id)
            .await
            .unwrap()
            .is_some()
    );
}

#[tokio::test]
async fn a_copy_of_an_earlier_checkpoint_marks_its_sessions_newer() {
    let (manager, repo) = exporter().await;
    let workspace = manager
        .create_workspace(&repo.id, "topic".into(), None, None, None)
        .await
        .unwrap();
    let first = manager
        .spaces
        .take_checkpoint(&*manager, &SpaceSelector::id(&workspace.id), None)
        .await
        .unwrap();
    fs::write(workspace.path.join("later"), "later\n").unwrap();
    let directory = tempfile::tempdir().unwrap();
    let file = directory.path().join("topic.shoal");

    let exported = manager
        .spaces
        .copy_space(
            &*manager,
            &SpaceSelector::id(&workspace.id),
            Some(first.number),
            file.clone(),
            &Carrying::Subtree,
        )
        .await
        .unwrap();
    assert_eq!(
        exported.manifest.copy,
        Some(Copied {
            earlier_files: true,
            held_elsewhere: false,
        })
    );
    let restored = import(&manager, &repo, &fs::read(&file).unwrap()).await;
    assert!(!restored.opened.workspace.path.join("later").exists());
    assert!(
        manager
            .spaces
            .copy_space(
                &*manager,
                &SpaceSelector::id(&workspace.id),
                Some(99.into()),
                directory.path().join("missing.shoal"),
                &Carrying::Subtree
            )
            .await
            .is_err()
    );
}

#[tokio::test]
async fn a_removed_workspace_has_nothing_to_copy() {
    let (manager, repo) = exporter().await;
    let workspace = manager
        .create_workspace(&repo.id, "topic".into(), None, None, None)
        .await
        .unwrap();
    manager
        .spaces
        .remove_selected(
            &*manager,
            &SpaceSelector::id(&workspace.id),
            UnsavedWork::All,
            InspectionPolicy::GitOnly,
        )
        .await
        .unwrap();
    let directory = tempfile::tempdir().unwrap();
    let refused = manager
        .spaces
        .copy_space(
            &*manager,
            &manager.named("topic").await,
            None,
            directory.path().join("topic.shoal"),
            &Carrying::Subtree,
        )
        .await
        .unwrap_err();
    assert!(
        format!("{refused:#}").contains("was removed"),
        "{refused:#}"
    );
}

#[tokio::test]
async fn concurrent_imports_of_a_copy_take_branches_of_their_own() {
    let (manager, repo) = exporter().await;
    let workspace = manager
        .create_workspace(&repo.id, "topic".into(), None, None, None)
        .await
        .unwrap();
    let directory = tempfile::tempdir().unwrap();
    let file = directory.path().join("topic.shoal");
    manager
        .spaces
        .copy_space(
            &*manager,
            &SpaceSelector::id(&workspace.id),
            None,
            file.clone(),
            &Carrying::Subtree,
        )
        .await
        .unwrap();
    let bytes = fs::read(&file).unwrap();

    let (first, second) = tokio::join!(
        import_here(&manager, &repo, &bytes),
        import_here(&manager, &repo, &bytes)
    );
    let mut branches =
        [first, second].map(|restored| restored.opened.workspace.branch().cloned().unwrap());
    branches.sort();
    assert_eq!(branches, ["topic-2", "topic-3"]);
}
