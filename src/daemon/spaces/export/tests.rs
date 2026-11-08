use std::{fs, os::unix::process::CommandExt, path::Path, sync::Arc};

use crate::client::space_names::SpaceSelector;
use crate::{
    daemon::{
        test_support::{manager, repository, unmarked},
        workspace::Manager,
    },
    process::identity,
    removal::{InspectionPolicy, UnsavedWork},
    transfer::{ExportedSpace, Import, Space},
};

mod transfer;

/// A manager with a chat `name` about a registered repository, whose
/// process this test reaps once a signal ends it, so the chat ends with it.
async fn chat(name: &str) -> (tempfile::TempDir, Arc<Manager>, u32) {
    chat_running(name, "exec sleep 6021").await
}

/// [`chat`] whose process runs the shell `script` with the chat's identity,
/// as a chat's agent runs: the chat starts with this process, then moves to
/// the script's.
async fn chat_running(name: &str, script: &str) -> (tempfile::TempDir, Arc<Manager>, u32) {
    let (root, manager) = manager().await;
    let checkout = repository(root.path(), "repo");
    let repo = manager
        .register_repository(checkout.to_str().unwrap().into(), None, None)
        .await
        .unwrap();
    let chat = manager
        .start_chat(
            repo.id.into(),
            name.into(),
            "claude".into(),
            std::process::id(),
            None,
            true,
        )
        .await
        .unwrap();
    // It leads its process group, as a chat's process does in a terminal.
    let mut process = unmarked("sh")
        .args(["-c", script])
        .env(crate::env::CHAT, &chat.id)
        .process_group(0)
        .spawn()
        .unwrap();
    let pid = process.id();
    std::thread::spawn(move || process.wait());
    manager.move_chat(&chat.id, pid).await.unwrap();
    (root, manager, pid)
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
            &SpaceSelector::parse(selector),
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
        (packaged.name.as_str(), packaged.agent.as_str()),
        ("plan-cache", "claude")
    );
    assert!(import.bundle.is_none());
    let sessions = import.manifest.sessions.as_ref().unwrap();
    assert_eq!(sessions.reported().collect::<Vec<_>>(), [("claude", "s1")]);
    assert_eq!(sessions.last_sessions[0].execution, chat.id);
}

#[tokio::test]
async fn a_chat_without_a_session_keeps_running() {
    let (root, manager, pid) = chat("talk").await;
    let file = root.path().join("talk.shoal");

    assert!(export(&manager, "talk", &file).await.is_err());
    let chat = manager.chats().await.unwrap().remove(0);
    assert!(identity::capture(pid).unwrap().is_some());
    assert!(!file.exists());
    manager.stop_chat(&chat.id).await.unwrap();
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
async fn a_chat_ends_with_what_its_process_starts_as_it_stops() {
    let directory = tempfile::tempdir().unwrap();
    let child = directory.path().join("late-pid");
    let ready = directory.path().join("ready-pid");
    // A process that starts another when told to stop, and says so once it
    // will.
    let script = format!(
        "trap 'sleep 6021 & echo $! > \"{}\"; exit 0' TERM; echo $$ > \"{}\"; \
         while :; do sleep 1; done",
        child.display(),
        ready.display()
    );
    let (root, manager, pid) = chat_running("forking", &script).await;
    assert_eq!(written_pid(&ready).await, pid);
    let chat = manager.chats().await.unwrap().remove(0);
    manager
        .record_chat_session(&chat.id, "s1".into())
        .await
        .unwrap();

    export(&manager, "forking", &root.path().join("forking.shoal"))
        .await
        .unwrap();
    // Written as it started, before the export finished; stopped, it is gone
    // once whichever process adopts orphans reaps it.
    let late = written_pid(&child).await;
    for _ in 0..400 {
        if identity::capture(late).unwrap().is_none() {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(25)).await;
    }
    let survivor = identity::capture(late).unwrap();
    // A survivor would hold the test runner's output open.
    if let Some(survivor) = &survivor {
        identity::signal(survivor, libc::SIGKILL).unwrap();
    }
    assert_eq!(survivor, None);
    assert_eq!(identity::capture(pid).unwrap(), None);
}

#[tokio::test]
async fn a_chat_ends_without_the_workspace_executions_it_started() {
    let directory = tempfile::tempdir().unwrap();
    let started = directory.path().join("wrapper-pid");
    // A command in the chat started a workspace's execution, whose wrapper
    // stays among the chat's processes.
    let script = format!(
        "sleep 6021 & echo $! > '{}'; while :; do sleep 1; done",
        started.display()
    );
    let (root, manager, _) = chat_running("coordinator", &script).await;
    let wrapper = identity::capture(written_pid(&started).await)
        .unwrap()
        .expect("the wrapper runs");
    let chat = manager.chats().await.unwrap().remove(0);
    let repository = chat.repository_id.clone().unwrap();
    let workspace = manager
        .create_workspace(&repository, "worker".into(), None, None, None)
        .await
        .unwrap();
    let recorded = serde_json::to_string(&wrapper).unwrap();
    manager
        .store
        .run(move |db| {
            db.execute(
                "INSERT INTO executions(id,workspace_id,state,wrapper)
                 VALUES ('worker-run',?1,'running',?2)",
                [&workspace.id, &recorded],
            )?;
            Ok(())
        })
        .await
        .unwrap();
    manager
        .record_chat_session(&chat.id, "s1".into())
        .await
        .unwrap();

    export(
        &manager,
        "coordinator",
        &root.path().join("coordinator.shoal"),
    )
    .await
    .unwrap();
    assert!(manager.chats().await.unwrap().is_empty());
    let kept = identity::alive(&wrapper).unwrap();
    identity::signal(&wrapper, libc::SIGKILL).unwrap();
    assert!(kept, "the workspace's execution was stopped with the chat");
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
            std::process::id(),
            None,
            false,
        )
        .await
        .unwrap();
    let file = root.path().join("worker-chat.shoal");
    assert!(export(&manager, &chat.name, &file).await.is_err());
    assert!(!file.exists());
    assert!(manager.chat_by_id(&chat.id).await.unwrap().is_some());
}

#[tokio::test]
async fn a_workspace_with_a_chat_in_it_is_not_moved() {
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
            "reading".into(),
            "claude".into(),
            std::process::id(),
            None,
            false,
        )
        .await
        .unwrap();
    let file = root.path().join("worker.shoal");
    assert!(export(&manager, &workspace.name, &file).await.is_err());
    assert!(!file.exists());
    assert!(
        manager
            .find_workspace(&workspace.id)
            .await
            .unwrap()
            .is_some()
    );
    assert!(manager.chat_by_id(&chat.id).await.unwrap().is_some());
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
            std::process::id(),
            None,
            false,
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
            &"plan-cache".into(),
            Some(1),
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
        .copy_space(&"plan-cache".into(), None, file.clone())
        .await
        .unwrap();
    assert!(exported.manifest.copy.is_some());
    assert_eq!(manager.chats().await.unwrap().len(), 1);
    assert!(identity::capture(pid).unwrap().is_some());
    let import = Import::read(&mut &fs::read(&file).unwrap()[..]).unwrap();
    assert!(matches!(import.manifest.space, Space::Chat(_)));
    let sessions = import.manifest.sessions.as_ref().unwrap();
    assert_eq!(sessions.reported().collect::<Vec<_>>(), [("claude", "s1")]);
    manager.stop_chat(&chat.id).await.unwrap();
}

#[tokio::test]
async fn a_chat_without_a_session_has_none_to_fork() {
    let (root, manager, _pid) = chat("talk").await;
    let file = root.path().join("talk.shoal");

    assert!(
        manager
            .copy_space(&"talk".into(), None, file.clone())
            .await
            .is_err()
    );
    assert!(!file.exists());
    let chat = manager.chats().await.unwrap().remove(0);
    manager.stop_chat(&chat.id).await.unwrap();
}
