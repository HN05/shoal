use std::{fs, os::unix::process::CommandExt, path::Path, sync::Arc};

use crate::{
    daemon::workspace::Manager,
    process::identity,
    removal::{InspectionPolicy, UnsavedWork},
    test_support::{manager, repository},
    transfer::{ExportedSpace, Import, Space},
};

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
    let mut process = std::process::Command::new("sh")
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
            selector,
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

    let error = export(&manager, "talk", &file).await.unwrap_err();
    assert!(
        error.to_string().contains("reported no session"),
        "{error:#}"
    );
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
    assert!(!identity::alive(&agent).unwrap());
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
    // once launchd or init reaps it.
    let late = written_pid(&child).await;
    for _ in 0..400 {
        if identity::capture(late).unwrap().is_none() {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(25)).await;
    }
    assert_eq!(identity::capture(late).unwrap(), None);
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
    let workspace = manager
        .create_workspace(&chat.repository_id, "worker".into(), None, None, None)
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
    assert!(
        error.contains("shoal chat repo --name unwritten --agent claude --session s1 continues it"),
        "{error}"
    );
    assert!(manager.chats().await.unwrap().is_empty());
    assert_eq!(identity::capture(pid).unwrap(), None);
    assert_eq!(fs::read_to_string(&file).unwrap(), "kept");
}
