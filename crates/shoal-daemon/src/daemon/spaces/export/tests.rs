use std::{fs, os::unix::process::CommandExt, path::Path, sync::Arc};

use crate::client::space_names::SpaceSelector;
use crate::{
    daemon::{
        test_support::{manager, repository, unmarked},
        workspace::Manager,
    },
    process::identity,
    removal::{InspectionPolicy, UnsavedWork},
    transfer::{CarriedChatExt, ExportedSpace, Import, ManifestExt, Space},
};

mod moving;
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
        .env(crate::env::CHAT, &chat.id)
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

/// Stop the agent of `chat`, as `shoal stop` does.
async fn stop(manager: &Manager, chat: &crate::model::Chat) {
    let request = crate::daemon::workspace::StopRequest::manual();
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
        )
        .await
}

#[tokio::test]
async fn a_chat_ends_into_a_package_that_continues_its_session() {
    let (root, manager, pid) = chat("plan-cache").await;
    let chat = manager.chats().await.unwrap().remove(0);
    manager
        .record_chat_session(&chat.id, "s1".into())
        .await
        .unwrap();
    let file = root.path().join("plan-cache.shoal");

    let exported = export(&manager, "plan-cache", &file).await.unwrap();
    assert!(exported.removal.is_none());
    assert!(manager.chats().await.unwrap().is_empty());
    assert_eq!(identity::capture(pid).unwrap(), None);
    let import = Import::read(&mut &fs::read(&file).unwrap()[..]).unwrap();
    let Space::Chat(packaged) = &import.manifest.space else {
        panic!("not a chat: {:?}", import.manifest);
    };
    assert_eq!(
        (packaged.name.as_str(), packaged.agent.name()),
        ("plan-cache", "claude")
    );
    assert!(import.bundle.is_none());
    let sessions = import.manifest.sessions.as_ref().unwrap();
    assert_eq!(
        sessions.reported().collect::<Vec<_>>(),
        [(&crate::agent::Agent::from("claude"), &"s1".into())]
    );
    assert_eq!(
        sessions.last_sessions[0].execution.as_str(),
        chat.id.as_str()
    );
}

#[tokio::test]
async fn a_chat_without_a_session_keeps_running() {
    let (root, manager, pid) = chat("talk").await;
    let file = root.path().join("talk.shoal");

    assert!(export(&manager, "talk", &file).await.is_err());
    let chat = manager.chats().await.unwrap().remove(0);
    assert!(identity::capture(pid).unwrap().is_some());
    assert!(!file.exists());
    stop(&manager, &chat).await;
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
    manager
        .record_chat_session(&chat.id, "s1".into())
        .await
        .unwrap();

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
    manager
        .record_chat_session(&chat.id, "s1".into())
        .await
        .unwrap();
    // A file the package would replace, which it never does.
    let file = root.path().join("unwritten.shoal");
    fs::write(&file, "kept").unwrap();

    let error = export(&manager, "unwritten", &file).await.unwrap_err();
    let error = format!("{error:#}");
    assert!(error.contains("--session s1"), "{error}");
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
async fn a_chat_in_a_workspace_is_not_moved() {
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
    let file = root.path().join("worker-chat.shoal");
    assert!(export(&manager, &chat.name, &file).await.is_err());
    assert!(!file.exists());
    assert!(manager.chat_by_id(&chat.id).await.unwrap().is_some());
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
        )
        .await
        .unwrap();
    assert!(exporting.chats().await.unwrap().is_empty());
    let bytes = fs::read(&file).unwrap();
    let import = Import::read(&mut &bytes[..]).unwrap();
    let carried: Vec<_> = (import.manifest.chats.iter())
        .map(|carried| &carried.chat.space_id)
        .collect();
    assert_eq!(carried.len(), 3, "{carried:?}");
    let position = |id| carried.iter().position(|carried| *carried == id).unwrap();
    assert!(position(&child.id) < position(&grandchild.id));
    transfer::import(&importing, &target_repo, &bytes).await;
    for carried in &import.manifest.chats {
        let sessions = (import.manifest).chat_sessions_on(carried, Some("elsewhere"), None);
        let chat = carried.chat_with(sessions);
        let repository = (&target_repo.id).into();
        importing.import_chat(&repository, chat).await.unwrap();
    }

    // Each opens again under its ID, stopped, where it ran.
    let importing = &importing;
    let place = |id| async move {
        let chat = importing.chat_by_id(id).await.unwrap().unwrap();
        assert!(!chat.running);
        let parent = chat.parent.map(|parent| parent.id);
        (parent, chat.workspace.map(|workspace| workspace.id))
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
        .split_workspace(
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
        .remove_workspace_taking(
            &SpaceSelector::id(&workspace.id),
            UnsavedWork::All,
            InspectionPolicy::GitOnly,
            &taking,
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
    manager
        .record_chat_session(&chat.id, "s1".into())
        .await
        .unwrap();
    let refused = manager
        .copy_space(
            &manager.named("plan-cache").await,
            Some(1.into()),
            root.path().join("refused.shoal"),
        )
        .await
        .unwrap_err();
    assert!(
        format!("{refused:#}").contains("no checkpoints"),
        "{refused:#}"
    );
    let file = root.path().join("plan-cache.shoal");

    let exported = manager
        .copy_space(&manager.named("plan-cache").await, None, file.clone())
        .await
        .unwrap();
    assert!(exported.manifest.copy.is_some());
    assert_eq!(manager.chats().await.unwrap().len(), 1);
    assert!(identity::capture(pid).unwrap().is_some());
    let import = Import::read(&mut &fs::read(&file).unwrap()[..]).unwrap();
    assert!(matches!(import.manifest.space, Space::Chat(_)));
    let sessions = import.manifest.sessions.as_ref().unwrap();
    assert_eq!(
        sessions.reported().collect::<Vec<_>>(),
        [(&crate::agent::Agent::from("claude"), &"s1".into())]
    );
    stop(&manager, &chat).await;
}

#[tokio::test]
async fn a_chat_without_a_session_has_none_to_fork() {
    let (root, manager, _pid) = chat("talk").await;
    let file = root.path().join("talk.shoal");

    assert!(
        manager
            .copy_space(&manager.named("talk").await, None, file.clone())
            .await
            .is_err()
    );
    assert!(!file.exists());
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
        manager
            .record_chat_session(&chat.id, session.into())
            .await
            .unwrap();
    }
    manager
        .remove_chat(&SpaceSelector::id(&chat.id), None)
        .await
        .unwrap();
    (root, manager, chat)
}

/// Export the removed space `id` to `file`.
async fn export_removed(
    manager: &Manager,
    id: &crate::client::space_ids::SpaceId,
    file: &Path,
) -> anyhow::Result<ExportedSpace> {
    manager
        .export_space(
            &SpaceSelector::id(id),
            UnsavedWork::default(),
            InspectionPolicy::GitOnly,
            file.to_owned(),
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
    let Space::Chat(packaged) = &import.manifest.space else {
        panic!("not a chat: {:?}", import.manifest);
    };
    assert_eq!(
        (packaged.name.as_str(), packaged.mode.as_str()),
        ("notes", "plan")
    );
    let sessions = import.manifest.sessions.as_ref().unwrap();
    assert_eq!(
        sessions.reported().collect::<Vec<_>>(),
        [(&crate::agent::Agent::from("claude"), &"s1".into())]
    );
    assert_eq!(
        sessions.last_sessions[0].execution.as_str(),
        chat.id.as_str()
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
async fn a_removed_chat_without_a_session_is_not_exported() {
    let (root, manager, chat) = removed_chat("talk", "chat", None).await;
    let file = root.path().join("talk.shoal");

    let refused = export_removed(&manager, &chat.id, &file).await.unwrap_err();
    let restore = format!("shoal restore {}", chat.id);
    assert!(format!("{refused:#}").contains(&restore), "{refused:#}");
    assert!(!file.exists());
    assert_eq!(manager.removed_spaces().await.unwrap().len(), 1);
}

#[tokio::test]
async fn a_chat_without_a_repository_moves_and_copies_for_root_dir() {
    let (root, manager) = manager().await;
    let chat = crate::daemon::test_support::chat(&manager, "lead", "orchestrate", None, None).await;
    manager
        .record_chat_session(&chat.id, "s1".into())
        .await
        .unwrap();
    let selector = SpaceSelector::id(&chat.id);
    let copy = root.path().join("copy.shoal");
    let moved = root.path().join("lead.shoal");

    manager
        .copy_space(&selector, None, copy.clone())
        .await
        .unwrap();
    let exported = manager
        .export_space(
            &selector,
            UnsavedWork::All,
            InspectionPolicy::GitOnly,
            moved.clone(),
        )
        .await
        .unwrap();
    assert!(exported.removal.is_none());
    assert!(manager.chats().await.unwrap().is_empty());
    for file in [&copy, &moved] {
        let import = Import::read(&mut &fs::read(file).unwrap()[..]).unwrap();
        let Space::Chat(packaged) = &import.manifest.space else {
            panic!("not a chat: {:?}", import.manifest);
        };
        assert!(packaged.outside_repositories);
        assert_eq!(import.manifest.remote_url, None);
    }
}
