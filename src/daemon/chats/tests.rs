use super::*;
use crate::{
    daemon::events::{ChatEventKind, EventItem, EventKind},
    model::SpaceRef,
    test_support::{manager, repository},
};

/// Chat starts, stops and removals, and workspace parent changes, in
/// journal order.
async fn chat_events(manager: &Manager) -> Vec<String> {
    manager
        .workspace_events(None, 100)
        .await
        .unwrap()
        .into_iter()
        .filter_map(|item| match item {
            EventItem::Chat(event) => Some(format!("{} {}", event.kind, event.chat.name)),
            EventItem::Event(event) if event.details.kind == EventKind::ParentChanged => {
                let parent = event.details.parent.flatten().map(|parent| parent.id);
                Some(format!("{} {parent:?}", event.details.name))
            }
            EventItem::Event(_) | EventItem::File(_) | EventItem::Gap { .. } => None,
        })
        .collect()
}

#[tokio::test]
async fn chats_stop_with_their_process_and_keep_the_workspaces_they_open() {
    let (temp, manager) = manager().await;
    let checkout = repository(temp.path(), "repo");
    let repo = manager
        .register_repository(checkout.to_str().unwrap().into(), None, None)
        .await
        .unwrap();
    let workspace = manager
        .create_workspace(&repo.id, "worker".into(), None, None, None)
        .await
        .unwrap();
    let process =
        crate::test_support::ProcessGroup::spawn(std::process::Command::new("sleep").arg("600"));
    let ending = manager
        .start_chat(
            repo.id.clone().into(),
            "stopping".into(),
            "claude".into(),
            process.id(),
            None,
            false,
        )
        .await
        .unwrap();
    let running = manager
        .start_chat(
            repo.id.clone().into(),
            "running".into(),
            "codex".into(),
            std::process::id(),
            None,
            false,
        )
        .await
        .unwrap();
    assert_eq!(
        (ending.name.as_str(), running.name.as_str()),
        ("stopping", "running")
    );
    assert_eq!(
        manager.chats().await.unwrap(),
        [ending.clone(), running.clone()]
    );

    manager
        .assign_chat(&workspace.id, Some(ending.id.clone()))
        .await
        .unwrap();
    assert_eq!(
        manager
            .workspace(&workspace.id)
            .await
            .unwrap()
            .parent
            .map(|parent| parent.id),
        Some(ending.id.clone())
    );
    // An unknown chat gives the workspace to the user rather than failing.
    manager
        .assign_chat(&workspace.id, Some("gone".into()))
        .await
        .unwrap();
    assert_eq!(
        manager
            .workspace(&workspace.id)
            .await
            .unwrap()
            .parent
            .map(|parent| parent.id),
        None
    );
    manager
        .assign_chat(&workspace.id, Some(ending.id.clone()))
        .await
        .unwrap();

    // The chat stops with its process, waiting for `shoal resume` with its
    // workspace and its name.
    process.kill();
    let worker = SpaceRef {
        id: workspace.id.clone(),
        name: workspace.name.clone(),
        kind: SpaceKind::Workspace,
        branch: workspace.branch.clone(),
    };
    let stopped = Chat {
        restore: Some(ChatRestore::Resume),
        children: vec![worker],
        ..ending.clone()
    };
    assert_eq!(
        manager.chats().await.unwrap(),
        [stopped.clone(), running.clone()]
    );
    assert_eq!(
        manager
            .workspace(&workspace.id)
            .await
            .unwrap()
            .parent
            .map(|parent| parent.id),
        Some(ending.id.clone())
    );
    let error = manager
        .check_chat_name("stopping".into())
        .await
        .unwrap_err();
    assert!(
        error.to_string().contains("already has the name"),
        "{error:#}"
    );
    assert_eq!(
        chat_events(&manager).await,
        [
            format!("{} stopping", ChatEventKind::Started),
            format!("{} running", ChatEventKind::Started),
            format!("worker {:?}", Some(&ending.id)),
            "worker None".to_owned(),
            format!("worker {:?}", Some(&ending.id)),
            format!("{} stopping", ChatEventKind::Saved),
        ]
    );
}

#[tokio::test]
async fn a_chat_needs_a_running_process() {
    let (temp, manager) = manager().await;
    let checkout = repository(temp.path(), "repo");
    let repo = manager
        .register_repository(checkout.to_str().unwrap().into(), None, None)
        .await
        .unwrap();
    let mut process = std::process::Command::new("true").spawn().unwrap();
    let pid = process.id();
    process.wait().unwrap();
    let error = manager
        .start_chat(
            repo.id.into(),
            "chat-4".into(),
            "claude".into(),
            pid,
            None,
            false,
        )
        .await
        .unwrap_err();
    assert!(error.to_string().contains("not running"), "{error:#}");
    assert!(manager.chats().await.unwrap().is_empty());
}

#[tokio::test]
async fn a_chat_keeps_the_session_its_agent_reports() {
    let (temp, manager) = manager().await;
    let checkout = repository(temp.path(), "repo");
    let repo = manager
        .register_repository(checkout.to_str().unwrap().into(), None, None)
        .await
        .unwrap();
    let chat = manager
        .start_chat(
            repo.id.into(),
            "reporter".into(),
            "claude".into(),
            std::process::id(),
            None,
            false,
        )
        .await
        .unwrap();
    assert_eq!(chat.session, None);
    let reported = manager
        .set_agent_state(
            "reporter",
            crate::state::AgentState::Idle,
            None,
            Some("session-1".into()),
        )
        .await
        .unwrap();
    assert_eq!(reported.state, crate::state::AgentState::Idle);
    // A report without a session, or with another, keeps the latest one.
    for session in [None, Some("session-2".into())] {
        manager
            .set_agent_state(&chat.id, crate::state::AgentState::Idle, None, session)
            .await
            .unwrap();
    }
    let chats = manager.chats().await.unwrap();
    assert_eq!(chats[0].session.as_deref(), Some("session-2"));
    let error = manager
        .set_agent_state(
            &chat.id,
            crate::state::AgentState::Idle,
            None,
            Some("../escape".into()),
        )
        .await
        .unwrap_err();
    assert!(error.to_string().contains("invalid"), "{error:#}");
}

#[tokio::test]
async fn a_moved_chat_runs_while_its_new_process_runs() {
    let (temp, manager) = manager().await;
    let checkout = repository(temp.path(), "repo");
    let repo = manager
        .register_repository(checkout.to_str().unwrap().into(), None, None)
        .await
        .unwrap();
    let chat = manager
        .start_chat(
            repo.id.into(),
            "chat-5".into(),
            "happy-claude".into(),
            std::process::id(),
            None,
            false,
        )
        .await
        .unwrap();
    let process =
        crate::test_support::ProcessGroup::spawn(std::process::Command::new("sleep").arg("600"));
    let moved = manager.move_chat(&chat.id, process.id()).await.unwrap();
    assert_eq!(
        moved,
        Chat {
            pid: process.id(),
            ..chat.clone()
        }
    );
    assert_eq!(manager.chats().await.unwrap(), [moved]);

    // The recording process still runs, but the chat stops with the new one.
    process.kill();
    assert!(manager.chats().await.unwrap()[0].restore.is_some());
    let error = manager
        .move_chat(&chat.id, std::process::id())
        .await
        .unwrap_err();
    assert!(error.to_string().contains("no running chat"), "{error:#}");
}

/// A process stop reads the tracked processes and stops the rest under the
/// handoff lock, so a chat's new process, a descendant of the launcher that
/// waits for its handoff, is never found untracked meanwhile.
#[tokio::test]
async fn a_chat_handoff_waits_for_a_process_stop_in_progress() {
    let (temp, manager) = manager().await;
    let checkout = repository(temp.path(), "repo");
    let repo = manager
        .register_repository(checkout.to_str().unwrap().into(), None, None)
        .await
        .unwrap();
    let chat = manager
        .start_chat(
            repo.id.into(),
            "chat-6".into(),
            "happy-claude".into(),
            std::process::id(),
            None,
            false,
        )
        .await
        .unwrap();
    let process =
        crate::test_support::ProcessGroup::spawn(std::process::Command::new("sleep").arg("600"));
    let stopping = manager.chat_handoffs.read().await;

    let handoff = tokio::spawn({
        let manager = manager.clone();
        let chat = chat.id.clone();
        let pid = process.id();
        async move { manager.move_chat(&chat, pid).await }
    });
    // The lock prefers writers, so new readers wait once the handoff queues.
    while manager.chat_handoffs.try_read().is_ok() {
        assert!(!handoff.is_finished());
        tokio::task::yield_now().await;
    }
    assert!(!handoff.is_finished());
    assert_eq!(manager.chats().await.unwrap()[0].pid, std::process::id());
    drop(stopping);

    assert_eq!(handoff.await.unwrap().unwrap().pid, process.id());
}

#[tokio::test]
async fn a_chat_moved_after_its_process_was_found_ended_keeps_running() {
    let (temp, manager) = manager().await;
    let checkout = repository(temp.path(), "repo");
    let repo = manager
        .register_repository(checkout.to_str().unwrap().into(), None, None)
        .await
        .unwrap();
    let recording =
        crate::test_support::ProcessGroup::spawn(std::process::Command::new("sleep").arg("600"));
    let chat = manager
        .start_chat(
            repo.id.into(),
            "chat-6".into(),
            "happy-claude".into(),
            recording.id(),
            None,
            false,
        )
        .await
        .unwrap();
    // A sweep finds the recording process ended...
    let recorded = manager
        .store
        .run(|db| recorded_processes(db))
        .await
        .unwrap();
    recording.kill();
    let exited = finished(recorded);
    assert_eq!(exited.len(), 1);
    // ...while the chat moves to its session's process.
    let moved = manager
        .move_chat(&chat.id, std::process::id())
        .await
        .unwrap();
    let released = manager
        .store
        .run(move |db| stop(db, &exited))
        .await
        .unwrap();
    assert!(released.is_empty());
    assert_eq!(manager.chats().await.unwrap(), [moved]);
}

#[tokio::test]
async fn chats_and_workspaces_share_one_namespace() {
    let (temp, manager) = manager().await;
    let checkout = repository(temp.path(), "repo");
    let repo = manager
        .register_repository(checkout.to_str().unwrap().into(), None, None)
        .await
        .unwrap();
    let worker = manager
        .create_workspace(&repo.id, "worker".into(), None, None, None)
        .await
        .unwrap();
    let start = |name: &str, pid| {
        manager.start_chat(
            repo.id.clone().into(),
            name.into(),
            "claude".into(),
            pid,
            None,
            false,
        )
    };
    let refusal = |result: Result<Chat>| format!("{:#}", result.unwrap_err());

    // A chat takes neither a workspace's name nor an invalid one.
    assert!(refusal(start("worker", std::process::id()).await).contains("workspace name"));
    assert!(refusal(start("-talk", std::process::id()).await).contains("chat names must"));
    let process =
        crate::test_support::ProcessGroup::spawn(std::process::Command::new("sleep").arg("600"));
    start("talk", process.id()).await.unwrap();
    assert!(
        refusal(start("talk", std::process::id()).await)
            .contains("a chat already has the name talk")
    );
    assert!(manager.check_chat_name("talk".into()).await.is_err());
    manager.check_chat_name("other".into()).await.unwrap();
    assert_eq!(
        manager.free_chat_name("talk".into()).await.unwrap(),
        "talk-2"
    );
    assert_eq!(
        manager.free_chat_name("worker".into()).await.unwrap(),
        "worker-2"
    );
    assert_eq!(
        manager.free_chat_name("re po".into()).await.unwrap(),
        "re-po"
    );

    // A workspace is neither checked, created nor renamed to a chat's name.
    let options = Default::default();
    let error = manager
        .check_creation(&repo.id, Some("talk"), None, options)
        .await
        .unwrap_err();
    assert!(
        format!("{error:#}").contains("a chat already has"),
        "{error:#}"
    );
    let error = manager
        .create_workspace(&repo.id, "talk".into(), None, None, None)
        .await
        .unwrap_err();
    assert!(
        format!("{error:#}").contains("a chat already has"),
        "{error:#}"
    );
    let error = manager
        .rename_workspace(&worker.id, "talk", None)
        .await
        .unwrap_err();
    assert!(
        format!("{error:#}").contains("a chat already has"),
        "{error:#}"
    );

    // The name frees once the chat is removed; a stopped chat keeps it.
    process.kill();
    manager.stop_finished_chats().await.unwrap();
    assert!(
        manager
            .check_creation(&repo.id, Some("talk"), None, options)
            .await
            .is_err()
    );
    manager.remove_chat("talk", None).await.unwrap();
    manager
        .check_creation(&repo.id, Some("talk"), None, options)
        .await
        .unwrap();
    let renamed = manager
        .rename_workspace(&worker.id, "talk", None)
        .await
        .unwrap();
    assert_eq!(renamed.name, "talk");
    assert!(refusal(start("talk", std::process::id()).await).contains("workspace name"));
}

#[tokio::test]
async fn a_renamed_chat_takes_a_free_name_with_its_files() {
    let (temp, manager) = manager().await;
    let checkout = repository(temp.path(), "repo");
    let repo = manager
        .register_repository(checkout.to_str().unwrap().into(), None, None)
        .await
        .unwrap();
    manager
        .create_workspace(&repo.id, "worker".into(), None, None, None)
        .await
        .unwrap();
    let start = |name: &str| {
        manager.start_chat(
            repo.id.clone().into(),
            name.into(),
            "happy-claude".into(),
            std::process::id(),
            None,
            false,
        )
    };
    let talk = start("talk").await.unwrap();
    start("other").await.unwrap();
    let files = manager.paths.chat_files(&talk);
    std::fs::create_dir_all(files.parent().unwrap()).unwrap();
    std::fs::write(files.with_extension("log"), "talk log").unwrap();
    std::fs::write(files.with_extension("prompt.md"), "talk prompt").unwrap();
    // An ended chat named topic left its log.
    let topic_log = files.with_file_name("topic.log");
    std::fs::write(&topic_log, "ended log").unwrap();
    let refusal = |result: Result<RenamedChat>| format!("{:#}", result.unwrap_err());

    // The new name is refused where a new chat's would be.
    let rename = |chat: &str, name: &str| manager.rename_chat(chat.into(), name.into());
    assert!(refusal(rename("talk", "worker").await).contains("workspace name"));
    assert!(refusal(rename("talk", "other").await).contains("a chat already has"));
    assert!(refusal(rename("talk", "-topic").await).contains("chat names must"));
    assert!(refusal(rename("gone", "topic").await).contains("no running chat gone"));
    assert_eq!(
        rename("talk", "talk").await.unwrap(),
        RenamedChat {
            chat: talk.clone(),
            previous_name: "talk".into(),
        }
    );

    // The reply names the chat as it was before.
    let renamed = rename(&talk.id, "topic").await.unwrap();
    assert_eq!(
        renamed,
        RenamedChat {
            chat: Chat {
                name: "topic".into(),
                ..talk.clone()
            },
            previous_name: "talk".into(),
        }
    );
    assert!(manager.chats().await.unwrap().contains(&renamed.chat));
    assert_eq!(std::fs::read_to_string(&topic_log).unwrap(), "talk log");
    assert_eq!(
        std::fs::read_to_string(files.with_file_name("topic.prompt.md")).unwrap(),
        "talk prompt"
    );
    assert!(!files.with_extension("log").exists());

    // The old name is free, and the rename is journaled.
    start("talk").await.unwrap();
    assert_eq!(
        chat_events(&manager).await,
        [
            format!("{} talk", ChatEventKind::Started),
            format!("{} other", ChatEventKind::Started),
            format!("{} topic", ChatEventKind::Renamed),
            format!("{} talk", ChatEventKind::Started),
        ]
    );
}

#[tokio::test]
async fn a_chat_keeps_its_name_until_its_session_takes_it_over() {
    let (temp, manager) = manager().await;
    let checkout = repository(temp.path(), "repo");
    let repo = manager
        .register_repository(checkout.to_str().unwrap().into(), None, None)
        .await
        .unwrap();
    let chat = manager
        .start_chat(
            repo.id.into(),
            "talk".into(),
            "happy-claude".into(),
            std::process::id(),
            None,
            true,
        )
        .await
        .unwrap();
    // Its launcher has yet to open the files named after it.
    let error = manager
        .rename_chat(chat.id.clone(), "topic".into())
        .await
        .unwrap_err();
    assert!(
        error.to_string().contains("talk is still starting"),
        "{error:#}"
    );
    manager
        .move_chat(&chat.id, std::process::id())
        .await
        .unwrap();
    let renamed = manager
        .rename_chat(chat.id.clone(), "topic".into())
        .await
        .unwrap();
    assert_eq!(renamed.chat.name, "topic");
}

#[tokio::test]
async fn concurrent_renames_keep_each_chats_files_under_its_name() {
    let (temp, manager) = manager().await;
    let checkout = repository(temp.path(), "repo");
    let repo = manager
        .register_repository(checkout.to_str().unwrap().into(), None, None)
        .await
        .unwrap();
    for round in 0..20 {
        let mut chats = Vec::new();
        for name in ["first", "second"] {
            let chat = manager
                .start_chat(
                    repo.id.clone().into(),
                    format!("{name}-{round}"),
                    "happy-claude".into(),
                    std::process::id(),
                    None,
                    false,
                )
                .await
                .unwrap();
            let log = manager.paths.chat_files(&chat).with_extension("log");
            std::fs::create_dir_all(log.parent().unwrap()).unwrap();
            std::fs::write(&log, &chat.id).unwrap();
            chats.push(chat);
        }
        // The second chat takes the first's name while the first leaves it,
        // in whichever order the two arrive.
        let (first, second) = (&chats[0], &chats[1]);
        let _ = tokio::join!(
            manager.rename_chat(first.id.clone(), format!("moved-{round}")),
            manager.rename_chat(second.id.clone(), first.name.clone()),
        );
        for chat in manager.chats().await.unwrap() {
            let log = manager.paths.chat_files(&chat).with_extension("log");
            assert_eq!(std::fs::read_to_string(&log).unwrap(), chat.id, "{chat:?}");
        }
    }
}

#[test]
fn suffixed_names_fit_the_name_limit() {
    let long = "a".repeat(crate::validate::MAX_NAME_LEN);
    assert_eq!(suffixed("chat", 1), "chat");
    assert_eq!(suffixed("chat", 3), "chat-3");
    let cut = suffixed(&long, 12);
    assert_eq!(cut.len(), crate::validate::MAX_NAME_LEN);
    assert!(cut.ends_with("a-12"), "{cut}");
}

#[tokio::test]
async fn a_chat_has_its_own_tmpdir_until_it_is_removed() {
    use std::os::unix::fs::PermissionsExt;
    let (temp, manager) = manager().await;
    let checkout = repository(temp.path(), "repo");
    let repo = manager
        .register_repository(checkout.to_str().unwrap().into(), None, None)
        .await
        .unwrap();
    let process =
        crate::test_support::ProcessGroup::spawn(std::process::Command::new("sleep").arg("600"));
    let chat = manager
        .start_chat(
            repo.id.clone().into(),
            "talk".into(),
            "claude".into(),
            process.id(),
            None,
            false,
        )
        .await
        .unwrap();
    let tmp_dir = chat.tmp_dir.clone().unwrap();
    let directory = repo.workspaces_dir.clone().unwrap();
    assert_eq!(tmp_dir, directory.join(".tmp").join(&chat.id));
    let mode = std::fs::metadata(&tmp_dir).unwrap().permissions().mode();
    assert_eq!(mode & 0o777, 0o700);
    assert_eq!(
        crate::env::chat_environment(&chat, "token", &manager.paths)[crate::env::TMPDIR],
        tmp_dir.as_os_str()
    );
    std::fs::write(tmp_dir.join("scratch"), "garbage").unwrap();

    // A stopped chat keeps it.
    process.kill();
    assert!(manager.chats().await.unwrap()[0].restore.is_some());
    assert!(tmp_dir.exists());
    manager.remove_chat(&chat.id, None).await.unwrap();
    assert!(manager.chats().await.unwrap().is_empty());
    assert!(!tmp_dir.exists());

    // A chat that cannot have one is forgotten at once.
    std::fs::remove_dir_all(directory.join(".tmp")).unwrap();
    std::fs::create_dir(directory.join(".tmp")).unwrap();
    let error = manager
        .start_chat(
            repo.id.clone().into(),
            "talk".into(),
            "claude".into(),
            std::process::id(),
            None,
            false,
        )
        .await
        .unwrap_err();
    assert!(
        error.to_string().contains("not a directory Shoal created"),
        "{error:#}"
    );
    assert!(manager.chats().await.unwrap().is_empty());
}

#[tokio::test]
async fn removing_a_chat_removes_the_workspaces_below_it_first_and_stops_its_agent() {
    let (temp, manager) = manager().await;
    let checkout = repository(temp.path(), "repo");
    let repo = manager
        .register_repository(checkout.to_str().unwrap().into(), None, None)
        .await
        .unwrap();
    let process =
        crate::test_support::ProcessGroup::spawn(std::process::Command::new("sleep").arg("600"));
    let chat = manager
        .start_chat(
            repo.id.clone().into(),
            "talk".into(),
            "claude".into(),
            process.id(),
            None,
            false,
        )
        .await
        .unwrap();
    let opened = manager
        .create_workspace_for_chat(
            repo.id.clone().into(),
            "opened".into(),
            None,
            Default::default(),
            Some(chat.id.clone()),
        )
        .await
        .unwrap();
    let split = manager
        .split_workspace(&opened.id, "split".into(), None)
        .await
        .unwrap();
    let other = manager
        .create_workspace(&repo.id, "other".into(), None, None, None)
        .await
        .unwrap();

    // A removal the user confirmed for fewer spaces than are below it takes
    // none of them, and the chat stays.
    let error = manager
        .remove_chat("talk", Some(vec![opened.id.clone()]))
        .await
        .unwrap_err();
    assert!(
        format!("{error:#}")
            .contains("split was opened below opened after the removal was confirmed"),
        "{error:#}"
    );
    assert!(manager.workspace(&split.id).await.is_ok());
    assert!(manager.recorded_chat("talk").await.unwrap().is_some());

    let removed = manager.remove_chat("talk", None).await.unwrap();
    assert_eq!(removed.chat.id, chat.id);
    assert_eq!(removed.children, ["split", "opened"]);
    assert!(manager.workspace(&opened.id).await.is_err());
    assert!(manager.workspace(&split.id).await.is_err());
    assert!(manager.workspace(&other.id).await.is_ok());
    assert!(manager.chats().await.unwrap().is_empty());
    // The removal stopped the chat's agent.
    assert!(!process_wait(process).success());
}

/// How `process` exited.
fn process_wait(mut process: crate::test_support::ProcessGroup) -> std::process::ExitStatus {
    process.wait().unwrap()
}

#[tokio::test]
async fn a_workspace_whose_creation_fails_stays_below_the_chat_that_opened_it() {
    let (temp, manager) = manager().await;
    let checkout = repository(temp.path(), "repo");
    let repo = manager
        .register_repository(checkout.to_str().unwrap().into(), None, None)
        .await
        .unwrap();
    let chat = manager
        .start_chat(
            repo.id.clone().into(),
            "talk".into(),
            "claude".into(),
            std::process::id(),
            None,
            false,
        )
        .await
        .unwrap();
    // A base that does not resolve fails the worktree after the record.
    let error = manager
        .create_workspace_for_chat(
            repo.id.clone().into(),
            "broken".into(),
            Some("does-not-exist".into()),
            Default::default(),
            Some(chat.id.clone()),
        )
        .await
        .unwrap_err();
    assert!(format!("{error:#}").contains("failed"), "{error:#}");
    // It is the chat's to inspect and remove, and goes with the chat.
    let broken = manager.workspace("broken").await.unwrap();
    assert_eq!(broken.state, crate::state::WorkspaceState::Failed);
    assert_eq!(broken.parent.map(|parent| parent.id), Some(chat.id));
}

#[tokio::test]
async fn a_chat_being_removed_takes_no_new_workspace() {
    let (temp, manager) = manager().await;
    let checkout = repository(temp.path(), "repo");
    let repo = manager
        .register_repository(checkout.to_str().unwrap().into(), None, None)
        .await
        .unwrap();
    let chat = manager
        .start_chat(
            repo.id.clone().into(),
            "talk".into(),
            "claude".into(),
            std::process::id(),
            None,
            false,
        )
        .await
        .unwrap();
    // A creation that reaches its record after the removal began is refused,
    // rather than leaving a workspace outside the chat.
    let _removal = manager.chat_removals.reserve(&chat.id).unwrap();
    let branch = manager
        .create_workspace_for_chat(
            repo.id.clone().into(),
            "late".into(),
            None,
            Default::default(),
            Some(chat.id.clone()),
        )
        .await
        .unwrap_err();
    let commit = manager
        .create_commit_workspace(
            repo.id.clone().into(),
            None,
            crate::protocol::CommitStart::Revision(None),
            Some(chat.id.clone()),
            Default::default(),
        )
        .await
        .unwrap_err();
    for error in [branch, commit] {
        assert!(format!("{error:#}").contains("being removed"), "{error:#}");
    }
    assert!(manager.list_workspaces().await.unwrap().is_empty());
}
