use std::{fs, os::unix::process::CommandExt, path::Path, sync::Arc};

use crate::{
    daemon::{
        manager::Manager,
        test_support::{manager, repository, unmarked},
    },
    model::RemovedSpace,
    process::identity,
    removal::{InspectionPolicy, UnsavedWork},
    transfer::{Carrying, ExportedSpace, Import, ManifestExt, PackagedSpaceExt, SpaceImport},
};
use shoal_protocol::space_names::SpaceSelector;

mod moving;
mod subtree;
mod transfer;

/// A manager with a chat `name` about a registered repository, whose
/// agent's process this test reaps once a signal ends it.
pub(super) async fn chat(name: &str) -> (tempfile::TempDir, Arc<Manager>, u32) {
    chat_running(name, "exec sleep 6021").await
}

/// [`chat`] whose agent's process runs the shell `script` with the chat's
/// identity, as the child its execution records.
async fn chat_running(name: &str, script: &str) -> (tempfile::TempDir, Arc<Manager>, u32) {
    let (root, manager) = manager().await;
    let checkout = repository(root.path(), "repo");
    let repo = manager
        .register_repository(checkout.to_str().unwrap().into(), None, None)
        .await
        .unwrap();
    let chat = manager
        .start_chat(repo.id.into(), name.into(), "claude".into(), None)
        .await
        .unwrap();
    // It leads its process group, as a chat's process does in a terminal.
    // Its output goes nowhere, so nothing it starts that outlives a killed
    // test holds the test runner's output open.
    let mut process = unmarked("sh")
        .args(["-c", script])
        .env(crate::env::SPACE, &chat.id)
        .process_group(0)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .unwrap();
    let pid = process.id();
    std::thread::spawn(move || process.wait());
    let execution = manager.workspace_executions(&chat.id).await.unwrap()[0]
        .id
        .clone();
    let child = identity::capture(pid).unwrap();
    manager
        .record_execution_child(execution, child, pid)
        .await
        .unwrap();
    (root, manager, pid)
}

/// What `shoal move import` sends to import the carried chat `chat` into
/// `repository`, or into none.
fn chat_import(repository: Option<&crate::model::Repository>, chat: RemovedSpace) -> SpaceImport {
    SpaceImport {
        repository: repository.map(|repository| (&repository.id).into()),
        bundle: None,
        prerequisite: None,
        space: chat,
        copy: false,
        session_files: Vec::new(),
        copies: Default::default(),
        carried: Vec::new(),
    }
}

/// Stop the agent of `chat`, as `shoal stop` does.
async fn stop(manager: &Manager, chat: &crate::model::Chat) {
    let request = crate::daemon::executions::StopRequest::manual();
    manager
        .stop_space(&SpaceSelector::id(&chat.id), request)
        .await
        .unwrap();
}

/// The process ID the chat's script writes to `file`, once it has.
async fn written_pid(file: &Path) -> u32 {
    for _ in 0..400 {
        if let Some(pid) = fs::read_to_string(file)
            .ok()
            .and_then(|pid| pid.trim().parse::<u32>().ok())
        {
            return pid;
        }
        tokio::time::sleep(std::time::Duration::from_millis(25)).await;
    }
    panic!("{} names no process", file.display());
}

async fn export(manager: &Manager, selector: &str, file: &Path) -> anyhow::Result<ExportedSpace> {
    manager
        .export_space(
            &manager.named(selector).await,
            UnsavedWork::All,
            InspectionPolicy::GitOnly,
            file.to_owned(),
            &Carrying::Subtree,
        )
        .await
}

#[tokio::test]
async fn a_chat_ends_into_a_package_that_continues_its_session() {
    let (root, manager, pid) = chat("plan-cache").await;
    let chat = manager.chats().await.unwrap().remove(0);
    crate::daemon::test_support::report_session(&manager, &chat.id, "s1").await;
    let file = root.path().join("plan-cache.shoal");

    let exported = export(&manager, "plan-cache", &file).await.unwrap();
    assert!(exported.removal.is_some_and(|removal| removal.removed));
    assert!(exported.warnings.is_empty(), "{:?}", exported.warnings);
    assert!(manager.chats().await.unwrap().is_empty());
    assert_eq!(identity::capture(pid).unwrap(), None);
    let import = Import::read(&mut &fs::read(&file).unwrap()[..]).unwrap();
    let packaged = &import.manifest.space.record;
    assert_eq!(packaged.space_id, chat.id);
    assert!(packaged.chat().is_some(), "a chat's record");
    assert_eq!(packaged.name.as_str(), "plan-cache");
    assert!(import.bundles.is_empty());
    let sessions = import.manifest.sessions.as_ref().unwrap();
    assert_eq!(
        sessions.reported().collect::<Vec<_>>(),
        [(&crate::agent::Agent::from("claude"), &"s1".into())]
    );
}

/// Whether `exported` warns that the space `name` saved no session.
fn warns_of(exported: &ExportedSpace, name: &str) -> bool {
    exported
        .warnings
        .iter()
        .any(|warning| warning.contains(name))
}

#[tokio::test]
async fn a_chat_without_a_session_ends_into_a_package_with_a_warning() {
    let (root, manager, pid) = chat("talk").await;
    let file = root.path().join("talk.shoal");

    let exported = export(&manager, "talk", &file).await.unwrap();
    assert!(warns_of(&exported, "talk"), "{:?}", exported.warnings);
    assert!(manager.chats().await.unwrap().is_empty());
    assert_eq!(identity::capture(pid).unwrap(), None);
    let import = Import::read(&mut &fs::read(&file).unwrap()[..]).unwrap();
    let sessions = import.manifest.sessions.as_ref().unwrap();
    assert_eq!(sessions.reported().count(), 0);
}

#[tokio::test]
async fn a_chat_ends_with_the_agent_its_launcher_started() {
    let directory = tempfile::tempdir().unwrap();
    let child = directory.path().join("agent-pid");
    // A launcher script that runs the agent as its child rather than
    // replacing itself with it.
    let script = format!("sleep 6021 & echo $! > '{}'; wait", child.display());
    let (root, manager, pid) = chat_running("launched", &script).await;
    let agent = identity::capture(written_pid(&child).await)
        .unwrap()
        .expect("the launcher's agent runs");
    let chat = manager.chats().await.unwrap().remove(0);
    crate::daemon::test_support::report_session(&manager, &chat.id, "s1").await;

    export(&manager, "launched", &root.path().join("launched.shoal"))
        .await
        .unwrap();
    let survived = identity::alive(&agent).unwrap();
    // A survivor would hold the test runner's output open.
    identity::signal(&agent, libc::SIGKILL).unwrap();
    assert!(!survived);
    assert_eq!(identity::capture(pid).unwrap(), None);
}

#[tokio::test]
async fn a_chat_whose_package_cannot_be_written_names_how_it_continues() {
    let (root, manager, pid) = chat("unwritten").await;
    let chat = manager.chats().await.unwrap().remove(0);
    crate::daemon::test_support::report_session(&manager, &chat.id, "s1").await;
    // A file the package would replace, which it never does.
    let file = root.path().join("unwritten.shoal");
    fs::write(&file, "kept").unwrap();

    let error = export(&manager, "unwritten", &file).await.unwrap_err();
    let error = format!("{error:#}");
    let restore = format!("shoal restore {}", chat.id);
    assert!(error.contains(&restore), "{error}");
    assert!(manager.chats().await.unwrap().is_empty());
    assert_eq!(identity::capture(pid).unwrap(), None);
    assert_eq!(fs::read_to_string(&file).unwrap(), "kept");
}

#[tokio::test]
async fn a_disposable_workspace_is_refused_before_its_removal() {
    let (temp, manager) = crate::daemon::test_support::manager().await;
    let checkout = crate::daemon::test_support::repository(temp.path(), "repo");
    let repo = manager
        .register_repository(checkout.to_str().unwrap().into(), None, None)
        .await
        .unwrap();
    let new = crate::daemon::workspace::NewWorkspace {
        options: crate::protocol::CreationOptions {
            disposable: true,
            ..Default::default()
        },
        ..Default::default()
    };
    let workspace = manager
        .create_workspace_with(&repo.id, "scratch".into(), None, new)
        .await
        .unwrap();
    let refused = export(
        &manager,
        &workspace.name,
        &temp.path().join("scratch.shoal"),
    )
    .await;
    assert!(refused.is_err());
    assert!(workspace.path.exists());
}

#[tokio::test]
async fn a_chat_in_a_workspace_moves_naming_the_workspace() {
    let (root, manager) = manager().await;
    let checkout = repository(root.path(), "repo");
    let repo = manager
        .register_repository(checkout.to_str().unwrap().into(), None, None)
        .await
        .unwrap();
    let workspace = manager
        .create_workspace(&repo.id, "worker".into(), None, None, None)
        .await
        .unwrap();
    let chat = manager
        .start_chat_in_workspace(
            &SpaceSelector::id(&workspace.id),
            "worker-chat".into(),
            "claude".into(),
            None,
        )
        .await
        .unwrap();
    crate::daemon::test_support::report_session(&manager, &chat.id, "s1").await;
    let file = root.path().join("worker-chat.shoal");

    export(&manager, &chat.name, &file).await.unwrap();
    let import = Import::read(&mut &fs::read(&file).unwrap()[..]).unwrap();
    let kept = import
        .manifest
        .space
        .record
        .chat()
        .expect("a chat's record");
    let ran_in = kept.workspace.as_ref().map(|workspace| &workspace.id);
    assert_eq!(ran_in, Some(&workspace.id));
    assert!(manager.chat_by_id(&chat.id).await.unwrap().is_none());
    assert!(workspace.path.exists());
}

#[tokio::test]
async fn a_workspace_moves_with_the_chats_in_it_and_below_it() {
    let (source, exporting, source_repo, origin) = transfer::exporter().await;
    let (_target, importing, target_repo) = transfer::importer(&origin).await;
    let workspace = exporting
        .create_workspace(&source_repo.id, "worker".into(), None, None, None)
        .await
        .unwrap();
    let selector = SpaceSelector::id(&workspace.id);
    let reading = exporting
        .start_chat_in_workspace(&selector, "reading".into(), "claude".into(), None)
        .await
        .unwrap();
    let chat = |name, parent| {
        crate::daemon::test_support::chat(&exporting, name, "chat", Some(&source_repo), parent)
    };
    let child = chat("child", Some(&workspace.id)).await;
    let grandchild = chat("grandchild", Some(&child.id)).await;
    let file = source.path().join("worker.shoal");

    exporting
        .export_space(
            &selector,
            UnsavedWork::All,
            InspectionPolicy::GitOnly,
            file.clone(),
            &Carrying::Subtree,
        )
        .await
        .unwrap();
    assert!(exporting.chats().await.unwrap().is_empty());
    let bytes = fs::read(&file).unwrap();
    let import = Import::read(&mut &bytes[..]).unwrap();
    let carried: Vec<_> = (import.manifest.carried.iter())
        .map(|carried| &carried.space.record.space_id)
        .collect();
    assert_eq!(carried.len(), 3, "{carried:?}");
    let position = |id| carried.iter().position(|carried| *carried == id).unwrap();
    assert!(position(&child.id) < position(&grandchild.id));
    transfer::import(&importing, &target_repo, &bytes).await;
    for carried in &import.manifest.carried {
        let sessions = (import.manifest).carried_sessions_on(carried, Some("elsewhere"), None);
        let chat = carried.space.record_with(sessions);
        let import = chat_import(Some(&target_repo), chat);
        importing.import_space(import).await.unwrap();
    }

    // Each opens again under its ID, stopped, where it ran.
    let importing = &importing;
    let place = |id| async move {
        let chat = importing.chat_by_id(id).await.unwrap().unwrap();
        assert!(!chat.running);
        let parent = chat.parent.map(|parent| parent.id);
        (
            parent,
            chat.workdir
                .workspace()
                .map(|workspace| workspace.id.clone()),
        )
    };
    assert_eq!(place(&reading.id).await, (None, Some(workspace.id.clone())));
    assert_eq!(place(&child.id).await, (Some(workspace.id.clone()), None));
    assert_eq!(place(&grandchild.id).await, (Some(child.id.clone()), None));
}

#[tokio::test]
async fn a_chat_that_starts_after_the_export_checked_keeps_everything() {
    let (root, manager) = manager().await;
    let checkout = repository(root.path(), "repo");
    let repo = manager
        .register_repository(checkout.to_str().unwrap().into(), None, None)
        .await
        .unwrap();
    let workspace = manager
        .create_workspace(&repo.id, "worker".into(), None, None, None)
        .await
        .unwrap();
    let split = manager
        .create_child(
            &SpaceSelector::id(&workspace.id),
            "split".into(),
            None,
            Default::default(),
        )
        .await
        .unwrap();
    // The export checked and listed what its removal takes, then a chat
    // started in the workspace below.
    let taking = [split.id.clone()];
    let chat = manager
        .start_chat_in_workspace(
            &SpaceSelector::id(&split.id),
            "reading".into(),
            "claude".into(),
            None,
        )
        .await
        .unwrap();
    let removed = manager
        .remove_selected_taking(
            &SpaceSelector::id(&workspace.id),
            UnsavedWork::All,
            InspectionPolicy::GitOnly,
            &taking,
            None,
        )
        .await;
    assert!(removed.is_err());
    assert!(manager.chat_by_id(&chat.id).await.unwrap().is_some());
    for kept in [&workspace, &split] {
        assert!(manager.find_workspace(&kept.id).await.unwrap().is_some());
        assert!(kept.path.exists());
    }
}

#[tokio::test]
async fn a_chat_copy_keeps_the_chat_running() {
    let (root, manager, pid) = chat("plan-cache").await;
    let chat = manager.chats().await.unwrap().remove(0);
    crate::daemon::test_support::report_session(&manager, &chat.id, "s1").await;
    let refused = manager
        .copy_space(
            &manager.named("plan-cache").await,
            Some(1.into()),
            root.path().join("refused.shoal"),
            &Carrying::Subtree,
        )
        .await
        .unwrap_err();
    assert!(
        format!("{refused:#}").contains("no checkpoints"),
        "{refused:#}"
    );
    let file = root.path().join("plan-cache.shoal");

    let exported = manager
        .copy_space(
            &manager.named("plan-cache").await,
            None,
            file.clone(),
            &Carrying::Subtree,
        )
        .await
        .unwrap();
    assert!(exported.manifest.copy.is_some());
    assert_eq!(manager.chats().await.unwrap().len(), 1);
    assert!(identity::capture(pid).unwrap().is_some());
    let import = Import::read(&mut &fs::read(&file).unwrap()[..]).unwrap();
    assert!(import.manifest.space.record.chat().is_some());
    let sessions = import.manifest.sessions.as_ref().unwrap();
    assert_eq!(
        sessions.reported().collect::<Vec<_>>(),
        [(&crate::agent::Agent::from("claude"), &"s1".into())]
    );
    stop(&manager, &chat).await;
}

#[tokio::test]
async fn a_chat_without_a_session_is_copied_with_a_warning() {
    let (root, manager, pid) = chat("talk").await;
    let file = root.path().join("talk.shoal");

    let exported = manager
        .copy_space(
            &manager.named("talk").await,
            None,
            file.clone(),
            &Carrying::Subtree,
        )
        .await
        .unwrap();
    assert!(warns_of(&exported, "talk"), "{:?}", exported.warnings);
    assert!(file.exists());
    assert!(identity::capture(pid).unwrap().is_some());
    let chat = manager.chats().await.unwrap().remove(0);
    stop(&manager, &chat).await;
}

/// A manager with a chat `name` in `mode` about a registered repository,
/// removed once its agent reported `session`, when it did.
async fn removed_chat(
    name: &str,
    mode: &str,
    session: Option<&str>,
) -> (tempfile::TempDir, Arc<Manager>, crate::model::Chat) {
    let (root, manager) = manager().await;
    let checkout = repository(root.path(), "repo");
    let repo = manager
        .register_repository(checkout.to_str().unwrap().into(), None, None)
        .await
        .unwrap();
    let chat = crate::daemon::test_support::chat(&manager, name, mode, Some(&repo), None).await;
    if let Some(session) = session {
        crate::daemon::test_support::report_session(&manager, &chat.id, session).await;
    }
    manager
        .remove_selected(
            &SpaceSelector::id(&chat.id),
            UnsavedWork::default(),
            InspectionPolicy::GitOnly,
        )
        .await
        .unwrap();
    (root, manager, chat)
}

/// Export the removed space `id` to `file`.
async fn export_removed(
    manager: &Manager,
    id: &shoal_ids::space_ids::SpaceId,
    file: &Path,
) -> anyhow::Result<ExportedSpace> {
    manager
        .export_space(
            &SpaceSelector::id(id),
            UnsavedWork::default(),
            InspectionPolicy::GitOnly,
            file.to_owned(),
            &Carrying::Subtree,
        )
        .await
}

#[tokio::test]
async fn a_removed_chat_exports_from_its_record_which_stays() {
    let (root, manager, chat) = removed_chat("notes", "plan", Some("s1")).await;
    let file = root.path().join("notes.shoal");

    let exported = export_removed(&manager, &chat.id, &file).await.unwrap();
    assert!(exported.removal.is_none());
    let import = Import::read(&mut &fs::read(&file).unwrap()[..]).unwrap();
    let packaged = &import.manifest.space.record;
    let kept = packaged.chat().expect("a chat's record");
    assert_eq!(
        (packaged.name.as_str(), kept.mode.as_str()),
        ("notes", "plan")
    );
    let sessions = import.manifest.sessions.as_ref().unwrap();
    assert_eq!(
        sessions.reported().collect::<Vec<_>>(),
        [(&crate::agent::Agent::from("claude"), &"s1".into())]
    );
    let kept = manager.removed_spaces().await.unwrap();
    assert_eq!(
        kept.iter()
            .map(|removed| &removed.space_id)
            .collect::<Vec<_>>(),
        [&chat.id]
    );
}

#[tokio::test]
async fn a_removed_chat_without_a_session_exports_with_a_warning() {
    let (root, manager, chat) = removed_chat("talk", "chat", None).await;
    let file = root.path().join("talk.shoal");

    let exported = export_removed(&manager, &chat.id, &file).await.unwrap();
    assert!(warns_of(&exported, "talk"), "{:?}", exported.warnings);
    assert!(file.exists());
    assert_eq!(manager.removed_spaces().await.unwrap().len(), 1);
}

#[tokio::test]
async fn a_chat_without_a_repository_moves_and_copies_for_root_dir() {
    let (root, manager) = manager().await;
    let chat = crate::daemon::test_support::chat(&manager, "lead", "orchestrate", None, None).await;
    crate::daemon::test_support::report_session(&manager, &chat.id, "s1").await;
    let selector = SpaceSelector::id(&chat.id);
    let copy = root.path().join("copy.shoal");
    let moved = root.path().join("lead.shoal");

    manager
        .copy_space(&selector, None, copy.clone(), &Carrying::Subtree)
        .await
        .unwrap();
    let exported = manager
        .export_space(
            &selector,
            UnsavedWork::All,
            InspectionPolicy::GitOnly,
            moved.clone(),
            &Carrying::Subtree,
        )
        .await
        .unwrap();
    assert!(exported.removal.is_some());
    assert!(manager.chats().await.unwrap().is_empty());
    for file in [&copy, &moved] {
        let import = Import::read(&mut &fs::read(file).unwrap()[..]).unwrap();
        let packaged = &import.manifest.space.record;
        assert!(packaged.chat().is_some());
        assert_eq!(packaged.repository_id, None);
        assert_eq!(import.manifest.remote_url, None);
    }
}

#[tokio::test]
async fn a_chat_without_a_repository_below_one_moves_with_it_into_root_dir() {
    let (root, manager) = manager().await;
    let lead = crate::daemon::test_support::chat(&manager, "lead", "orchestrate", None, None).await;
    crate::daemon::test_support::report_session(&manager, &lead.id, "s1").await;
    let helper =
        crate::daemon::test_support::chat(&manager, "helper", "orchestrate", None, Some(&lead.id))
            .await;
    let file = root.path().join("lead.shoal");
    let selector = SpaceSelector::id(&lead.id);
    (manager.export_space(
        &selector,
        UnsavedWork::All,
        InspectionPolicy::GitOnly,
        file.clone(),
        &Carrying::Subtree,
    ))
    .await
    .unwrap();
    let import = Import::read(&mut &fs::read(&file).unwrap()[..]).unwrap();
    let [carried] = import.manifest.carried.as_slice() else {
        panic!("one chat goes with it: {:?}", import.manifest.carried);
    };
    let (_other, importing) = crate::daemon::test_support::manager().await;

    let sessions = (import.manifest).carried_sessions_on(carried, Some("elsewhere"), None);
    (importing.import_space(chat_import(None, carried.space.record_with(sessions))))
        .await
        .unwrap();

    let restored = importing.chat_by_id(&helper.id).await.unwrap().unwrap();
    let root_dir = importing.config().root_dir(&importing.paths).unwrap();
    assert_eq!(
        (
            restored.repository_id,
            restored.workdir.root_dir().map(Into::into)
        ),
        (None, Some(root_dir))
    );
}

#[tokio::test]
async fn a_carried_chat_whose_worktree_is_not_here_opens_in_the_checkout() {
    let (_source, exporting, source_repo, origin) = transfer::exporter().await;
    let (_target, importing, target_repo) = transfer::importer(&origin).await;
    let workspace = exporting
        .create_workspace(&source_repo.id, "worker".into(), None, None, None)
        .await
        .unwrap();
    let reading = exporting
        .start_chat_in_workspace(
            &SpaceSelector::id(&workspace.id),
            "reading".into(),
            "claude".into(),
            None,
        )
        .await
        .unwrap();
    crate::daemon::test_support::report_session(&exporting, &reading.id, "s1").await;
    (exporting.remove_selected(
        &SpaceSelector::id(&reading.id),
        UnsavedWork::default(),
        InspectionPolicy::GitOnly,
    ))
    .await
    .unwrap();
    let removed = (exporting.removed_spaces().await.unwrap().into_iter())
        .find(|removed| removed.space_id == reading.id)
        .unwrap();

    let restored = (importing.import_space(chat_import(Some(&target_repo), removed)))
        .await
        .unwrap();

    let crate::model::RestoredSpace::Chat(restored) = restored else {
        panic!("not a chat");
    };
    assert_eq!(restored.chat.workdir.workspace(), None);
    assert_eq!(restored.warnings.len(), 1, "{:?}", restored.warnings);
}

#[tokio::test]
async fn an_imported_chat_keeps_its_id_and_a_copy_takes_its_own() {
    let (_source, exporting, source_repo, origin) = transfer::exporter().await;
    let (_target, importing, target_repo) = transfer::importer(&origin).await;
    let (_copies, copying, copy_repo) = transfer::importer(&origin).await;
    let reading =
        crate::daemon::test_support::chat(&exporting, "reading", "chat", Some(&source_repo), None)
            .await;
    (exporting.remove_selected(
        &SpaceSelector::id(&reading.id),
        UnsavedWork::default(),
        InspectionPolicy::GitOnly,
    ))
    .await
    .unwrap();
    let removed = exporting.removed_spaces().await.unwrap().remove(0);

    let moved = (importing.import_space(chat_import(Some(&target_repo), removed.clone())))
        .await
        .unwrap();
    let copy = SpaceImport {
        copy: true,
        ..chat_import(Some(&copy_repo), removed)
    };
    let copied = copying.import_space(copy).await.unwrap();

    let id = |restored| match restored {
        crate::model::RestoredSpace::Chat(restored) => restored.chat.id,
        crate::model::RestoredSpace::Workspace(_) => panic!("not a chat"),
    };
    assert_eq!(id(moved), reading.id);
    assert_ne!(id(copied), reading.id);
}

#[tokio::test]
async fn a_moved_chat_keeps_its_id_with_the_chats_it_carried_below_it() {
    let (source, exporting, source_repo, origin) = transfer::exporter().await;
    let (_target, importing, target_repo) = transfer::importer(&origin).await;
    let start = |name, parent| {
        crate::daemon::test_support::chat(&exporting, name, "chat", Some(&source_repo), parent)
    };
    let lead = start("lead", None).await;
    let helper = start("helper", Some(&lead.id)).await;
    for chat in [&lead, &helper] {
        let session = format!("s-{}", chat.name);
        crate::daemon::test_support::report_session(&exporting, &chat.id, &session).await;
    }
    let file = source.path().join("lead.shoal");
    let selector = SpaceSelector::id(&lead.id);
    (exporting.export_space(
        &selector,
        UnsavedWork::All,
        InspectionPolicy::GitOnly,
        file.clone(),
        &Carrying::Subtree,
    ))
    .await
    .unwrap();

    let import = Import::read(&mut &fs::read(&file).unwrap()[..]).unwrap();
    let sessions = (import.manifest).sessions_on(Some("elsewhere"), None);
    let record = crate::transfer::PackagedSpaceExt::record_with(&import.manifest.space, sessions);
    (importing.import_space(chat_import(Some(&target_repo), record)))
        .await
        .unwrap();
    for carried in &import.manifest.carried {
        let sessions = (import.manifest).carried_sessions_on(carried, Some("elsewhere"), None);
        let chat = chat_import(Some(&target_repo), carried.space.record_with(sessions));
        importing.import_space(chat).await.unwrap();
    }

    let imported = importing.chat_by_id(&lead.id).await.unwrap().unwrap();
    assert!(!imported.running);
    let below = importing.chat_by_id(&helper.id).await.unwrap().unwrap();
    assert_eq!(below.parent.map(|parent| parent.id), Some(lead.id));
}

#[tokio::test]
async fn an_import_under_the_id_of_a_space_here_is_refused_before_recording_it() {
    let (root, manager, chat) = removed_chat("notes", "chat", Some("s1")).await;
    let file = root.path().join("notes.shoal");
    export_removed(&manager, &chat.id, &file).await.unwrap();
    // Its record stays, so it can open here again before the import.
    let selector = SpaceSelector::id(&chat.id);
    manager.restore_space(&selector, None).await.unwrap();
    let import = Import::read(&mut &fs::read(&file).unwrap()[..]).unwrap();
    let repository = (chat.repository_id.as_ref()).map(|id| id.clone().into());
    let space = SpaceImport {
        repository,
        ..chat_import(None, import.manifest.space.record)
    };

    let refused = manager.import_space(space).await.unwrap_err();

    assert_eq!(
        crate::refusal::Reason::of(&refused),
        Some(crate::refusal::Reason::NotImported)
    );
    assert!(manager.removed_spaces().await.unwrap().is_empty());
    assert!(manager.chat_by_id(&chat.id).await.unwrap().is_some());
}
