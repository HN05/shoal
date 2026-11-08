use crate::client::space_names::SpaceSelector;
use crate::process::identity::IdentityExt;
use crate::{
    daemon::{
        events::EventItem,
        test_support::{ProcessGroup, manager, repository, unmarked},
        workspace::{Ending, Manager},
    },
    model::{Chat, Repository},
    state::ChatRestore,
};

/// A registered repository and the chat `talk` running in it as a process
/// that lives until stopped.
pub(in crate::daemon::chats) async fn running_chat(
    root: &std::path::Path,
    manager: &Manager,
) -> (Repository, Chat, ProcessGroup) {
    named_chat(root, manager, "talk").await
}

/// [`running_chat`] named `name`.
pub(in crate::daemon::chats) async fn named_chat(
    root: &std::path::Path,
    manager: &Manager,
    name: &str,
) -> (Repository, Chat, ProcessGroup) {
    let checkout = repository(root, "repo");
    let repo = manager
        .register_repository(checkout.to_str().unwrap().into(), None, None)
        .await
        .unwrap();
    let process = ProcessGroup::spawn(unmarked("sleep").arg("600"));
    let chat = manager
        .start_chat(
            repo.id.clone().into(),
            name.into(),
            "claude".into(),
            process.id(),
            None,
            false,
        )
        .await
        .unwrap();
    (repo, chat, process)
}

/// The chat events of the journal, as `<kind> <name>`.
async fn chat_events(manager: &Manager) -> Vec<String> {
    manager
        .workspace_events(None, 100)
        .await
        .unwrap()
        .into_iter()
        .filter_map(|item| match item {
            EventItem::Chat(event) => Some(format!("{} {}", event.kind, event.chat.name)),
            EventItem::Event(_) | EventItem::File(_) | EventItem::Gap { .. } => None,
        })
        .collect()
}

#[tokio::test]
async fn a_saved_chat_keeps_its_record_until_it_is_restored() {
    let (root, manager) = manager().await;
    let (repo, chat, mut process) = running_chat(root.path(), &manager).await;
    let workspace = manager
        .create_workspace(&repo.id, "worker".into(), None, None, None)
        .await
        .unwrap();
    manager
        .assign_parent(&workspace.id, Some(chat.id.clone()))
        .await
        .unwrap();
    let tmp_dir = chat.tmp_dir.clone().unwrap();
    std::fs::write(tmp_dir.join("notes"), "kept").unwrap();

    manager.save_chats(ChatRestore::Automatic).await;

    assert!(!process.wait().unwrap().success());
    let saved = manager.chats().await.unwrap();
    assert_eq!(saved.len(), 1);
    assert_eq!(saved[0].restore, Some(ChatRestore::Automatic));
    assert!(manager.running_chat(&"talk".into()).await.is_err());
    assert_eq!(
        manager
            .workspace_by_id(&workspace.id)
            .await
            .unwrap()
            .parent
            .map(|parent| parent.id),
        Some(chat.id.clone())
    );
    assert!(manager.check_chat_name("talk".into()).await.is_err());
    assert_eq!(
        std::fs::read_to_string(tmp_dir.join("notes")).unwrap(),
        "kept"
    );

    let restored = manager
        .restore_chat(&chat.id, std::process::id(), None, false)
        .await
        .unwrap();
    assert_eq!(
        (
            restored.id.as_str(),
            restored.name.as_str(),
            restored.restore
        ),
        (chat.id.as_str(), "talk", None)
    );
    assert_eq!(restored.tmp_dir.as_deref(), Some(tmp_dir.as_path()));
    assert_eq!(manager.chats().await.unwrap(), [restored]);
    assert_eq!(
        std::fs::read_to_string(tmp_dir.join("notes")).unwrap(),
        "kept"
    );
    assert!(
        manager
            .restore_chat(&chat.id, std::process::id(), None, false)
            .await
            .is_err()
    );
    assert_eq!(
        chat_events(&manager).await,
        ["chat_started talk", "chat_saved talk", "chat_restored talk"]
    );
}

#[tokio::test]
async fn a_removed_saved_chat_ends() {
    let (root, manager) = manager().await;
    let (_repo, chat, _process) = running_chat(root.path(), &manager).await;

    manager.save_chats(ChatRestore::Resume).await;
    let removed = manager.remove_chat(&"talk".into(), None).await.unwrap();

    assert_eq!(removed.chat.id, chat.id);
    assert!(manager.chats().await.unwrap().is_empty());
    assert!(!chat.tmp_dir.unwrap().exists());
    manager.check_chat_name("talk".into()).await.unwrap();
    assert_eq!(
        chat_events(&manager).await,
        ["chat_started talk", "chat_saved talk", "chat_ended talk"]
    );
}

#[tokio::test]
async fn the_daemon_saves_running_chats_as_it_exits() {
    for (ending, restore) in [
        (Ending::Stop, ChatRestore::Resume),
        (Ending::Save, ChatRestore::Automatic),
    ] {
        let (root, manager) = manager().await;
        let (_repo, chat, mut process) = running_chat(root.path(), &manager).await;

        manager.end_work(ending).await;

        let saved = manager.chat_by_id(&chat.id).await.unwrap().unwrap();
        assert_eq!(saved.restore, Some(restore), "{ending:?}");
        assert!(!process.wait().unwrap().success());
    }
}

#[tokio::test]
async fn a_detaching_daemon_leaves_chats_running() {
    let (root, manager) = manager().await;
    let (_repo, chat, _process) = running_chat(root.path(), &manager).await;

    manager.end_work(Ending::Detach).await;

    assert!(
        manager
            .running_chat(&SpaceSelector::id(&chat.id))
            .await
            .is_ok()
    );
}

#[tokio::test]
async fn a_saved_chat_whose_process_runs_on_is_not_restored() {
    let (root, manager) = manager().await;
    let (repo, chat, _process) = running_chat(root.path(), &manager).await;
    let opened = manager
        .create_workspace_below(
            repo.id.into(),
            "opened".into(),
            None,
            Default::default(),
            Some(chat.id.clone()),
        )
        .await
        .unwrap();
    // As a save whose stop never ended the agent left it.
    manager
        .store
        .run(|db| {
            db.execute("UPDATE chats SET restore='resume'", [])?;
            Ok(())
        })
        .await
        .unwrap();

    assert!(
        manager
            .restore_chat(&chat.id, std::process::id(), None, false)
            .await
            .is_err()
    );
    let saved = manager.chat_by_id(&chat.id).await.unwrap().unwrap();
    assert_eq!(saved.pid, chat.pid);
    // Nor is it removed, which would delete the files its agent uses, and
    // the workspace below it stays too.
    assert!(manager.remove_chat(&"talk".into(), None).await.is_err());
    assert!(chat.tmp_dir.unwrap().exists());
    assert!(manager.workspace_by_id(&opened.id).await.is_ok());
}

#[tokio::test]
async fn a_restore_whose_agent_did_not_start_waits_for_resume_again() {
    let (root, manager) = manager().await;
    let (_repo, chat, mut process) = running_chat(root.path(), &manager).await;
    manager.save_chats(ChatRestore::Automatic).await;
    process.wait().unwrap();
    let restoring = ProcessGroup::spawn(unmarked("sleep").arg("600"));
    let restoring_pid = restoring.id();
    manager
        .restore_chat(&chat.id, restoring_pid, None, false)
        .await
        .unwrap();

    manager
        .return_to_saved(&chat.id, restoring_pid, "the agent is not installed", None)
        .await
        .unwrap();
    restoring.kill();

    let saved = manager.chats().await.unwrap();
    assert_eq!(saved.len(), 1);
    assert_eq!(saved[0].restore, Some(ChatRestore::Resume));
    assert!(chat.tmp_dir.unwrap().exists());
    let notifications = manager.notifications(true, 10).await.unwrap();
    assert!(
        notifications.iter().any(|notification| {
            notification.kind == crate::daemon::notifications::NotificationKind::ChatRestoreFailed
                && notification.message.contains("the agent is not installed")
        }),
        "{notifications:?}"
    );
    // Another process cannot return it.
    manager
        .restore_chat(&chat.id, std::process::id(), None, false)
        .await
        .unwrap();
    assert!(
        manager
            .return_to_saved(&chat.id, 1, "unused", None)
            .await
            .is_err()
    );
}

#[tokio::test]
async fn a_restore_that_failed_before_taking_the_chat_tells_the_user() {
    let (root, manager) = manager().await;
    let (_repo, chat, mut process) = running_chat(root.path(), &manager).await;
    manager.save_chats(ChatRestore::Resume).await;
    process.wait().unwrap();

    manager
        .return_to_saved(&chat.id, std::process::id(), "no such repository", None)
        .await
        .unwrap();

    let saved = manager.chat_by_id(&chat.id).await.unwrap().unwrap();
    assert_eq!(saved.restore, Some(ChatRestore::Resume));
    let notifications = manager.notifications(true, 10).await.unwrap();
    assert!(
        notifications.iter().any(|notification| {
            notification.kind == crate::daemon::notifications::NotificationKind::ChatRestoreFailed
                && notification.message.contains("no such repository")
        }),
        "{notifications:?}"
    );
}

#[tokio::test]
async fn a_stop_that_failed_after_the_agent_ended_keeps_the_chat_saved() {
    let (root, manager) = manager().await;
    let (_repo, chat, process) = running_chat(root.path(), &manager).await;
    let identity = crate::process::identity::capture(chat.pid)
        .unwrap()
        .unwrap();
    mark_saved(&manager).await;
    // As a stop that ended the agent, then failed on what it started.
    process.kill();

    manager
        .keep_after_failed_stop(chat.id.clone(), identity)
        .await;

    let saved = manager.chats().await.unwrap();
    assert_eq!(saved.len(), 1);
    assert_eq!(saved[0].restore, Some(ChatRestore::Resume));
    assert!(chat.tmp_dir.unwrap().exists());
}

#[tokio::test]
async fn a_stop_that_failed_while_the_agent_runs_leaves_the_chat_running() {
    let (root, manager) = manager().await;
    let (_repo, chat, _process) = running_chat(root.path(), &manager).await;
    let identity = crate::process::identity::capture(chat.pid)
        .unwrap()
        .unwrap();
    mark_saved(&manager).await;

    manager
        .keep_after_failed_stop(chat.id.clone(), identity)
        .await;

    assert!(
        manager
            .running_chat(&SpaceSelector::id(&chat.id))
            .await
            .is_ok()
    );
}

#[tokio::test]
async fn a_survivor_in_the_chats_process_group_holds_it_saved() {
    let (root, manager) = manager().await;
    let checkout = repository(root.path(), "repo");
    let repo = manager
        .register_repository(checkout.to_str().unwrap().into(), None, None)
        .await
        .unwrap();
    // The agent leads its group and starts a child without the chat's
    // identity, as a command it ran.
    let started = root.path().join("child");
    let mut leader = ProcessGroup::spawn(
        unmarked("sh")
            .arg("-c")
            .arg("sleep 600 & echo $! > \"$0\"; wait")
            .arg(&started),
    );
    let group = KilledGroup(leader.id());
    let chat = manager
        .start_chat(
            repo.id.clone().into(),
            "talk".into(),
            "claude".into(),
            leader.id(),
            None,
            false,
        )
        .await
        .unwrap();
    let child = wait_for_child(&started).await;
    mark_saved(&manager).await;
    // A stop interrupted after the agent ended leaves the child running.
    unsafe { libc::kill(leader.id() as i32, libc::SIGKILL) };
    leader.wait().unwrap();

    let error = manager
        .restore_chat(&chat.id, std::process::id(), None, false)
        .await
        .unwrap_err();
    assert!(error.to_string().contains(&child.to_string()), "{error:#}");
    let error = manager.remove_chat(&"talk".into(), None).await.unwrap_err();
    assert!(error.to_string().contains(&child.to_string()), "{error:#}");
    assert!(chat.tmp_dir.unwrap().exists());
    drop(group);
}

/// Mark every chat saved for `shoal resume`, as a save does before its stop.
async fn mark_saved(manager: &Manager) {
    manager
        .store
        .run(|db| {
            db.execute("UPDATE chats SET restore='resume'", [])?;
            Ok(())
        })
        .await
        .unwrap();
}

/// The PID the leader wrote to `file` once it started its child.
async fn wait_for_child(file: &std::path::Path) -> u32 {
    // A hang guard only: the shell writes it at once.
    for _ in 0..600 {
        if let Some(pid) = std::fs::read_to_string(file)
            .ok()
            .and_then(|pid| pid.trim().parse().ok())
        {
            return pid;
        }
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    }
    panic!("the child did not start");
}

/// Kills the process group `0` names once dropped, as the members a test's
/// reaped leader leaves.
struct KilledGroup(u32);

impl Drop for KilledGroup {
    fn drop(&mut self) {
        // SAFETY: the group's ID names no other group while a member lives.
        unsafe { libc::kill(-(self.0 as i32), libc::SIGKILL) };
    }
}

/// A child that left the agent's group and carries no chat identity is
/// found by the lineage of the agent's never-reused ID, which macOS reports.
#[cfg(target_os = "macos")]
#[tokio::test]
async fn a_survivor_that_left_the_chats_group_holds_it_saved() {
    let (root, manager) = manager().await;
    let checkout = repository(root.path(), "repo");
    let repo = manager
        .register_repository(checkout.to_str().unwrap().into(), None, None)
        .await
        .unwrap();
    let started = root.path().join("child");
    let mut leader = ProcessGroup::spawn(
        unmarked("sh")
            .arg("-c")
            .arg("perl -MPOSIX -e 'POSIX::setsid(); sleep 600' & echo $! > \"$0\"; wait")
            .arg(&started),
    );
    let chat = manager
        .start_chat(
            repo.id.clone().into(),
            "talk".into(),
            "claude".into(),
            leader.id(),
            None,
            false,
        )
        .await
        .unwrap();
    let child = wait_for_child(&started).await;
    // The child leads a session of its own, out of the agent's group.
    let escaped = KilledGroup(child);
    while unsafe { libc::getpgid(child as i32) } != child as i32 {
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    }
    // A save interrupted before its stop, then the agent ended.
    manager
        .store
        .run(|db| super::save(db, ChatRestore::Resume, None, None))
        .await
        .unwrap();
    unsafe { libc::kill(leader.id() as i32, libc::SIGKILL) };
    leader.wait().unwrap();

    let error = manager.remove_chat(&"talk".into(), None).await.unwrap_err();
    assert!(error.to_string().contains(&child.to_string()), "{error:#}");
    assert!(chat.tmp_dir.unwrap().exists());
    drop(escaped);
}

#[tokio::test]
async fn a_chat_whose_agent_exited_before_the_save_waits_for_resume() {
    let (root, manager) = manager().await;
    let (_repo, chat, process) = running_chat(root.path(), &manager).await;
    process.kill();

    manager.save_chats(ChatRestore::Automatic).await;

    let chats = manager.chats().await.unwrap();
    assert_eq!(chats[0].restore, Some(ChatRestore::Resume));
    assert!(chat.tmp_dir.unwrap().exists());
    assert_eq!(
        chat_events(&manager).await,
        ["chat_started talk", "chat_saved talk"]
    );
}

#[test]
fn a_group_under_the_agents_id_is_its_own_only_while_nobody_took_the_id() {
    use crate::daemon::chats::processes::led_group;
    use crate::process::identity::{Identity, capture};
    let live = Identity {
        unique: None,
        ..capture(std::process::id()).unwrap().unwrap()
    };
    assert_eq!(led_group(&live).unwrap(), Some(live.pid));
    // Its recorded lineage finds what it started instead, whoever took the
    // group's ID since.
    if let Some(unique) = capture(std::process::id()).unwrap().unwrap().unique {
        let traced = Identity {
            unique: Some(unique),
            ..live.clone()
        };
        assert_eq!(led_group(&traced).unwrap(), None);
    }
    // Another process now has the ID.
    let reused = Identity {
        birth: "an earlier process".into(),
        unique: None,
        ..live.clone()
    };
    assert_eq!(led_group(&reused).unwrap(), None);
    // After a reboot, any group under the ID is another's.
    assert_eq!(
        led_group(&live.recorded_in("an-earlier-boot")).unwrap(),
        None
    );
}

#[tokio::test]
async fn a_restore_during_a_shutdown_leaves_the_chat_saved() {
    let (root, manager) = manager().await;
    let (_repo, chat, mut process) = running_chat(root.path(), &manager).await;
    manager.save_chats(ChatRestore::Automatic).await;
    process.wait().unwrap();
    // A restore claimed earlier arrives once the shutdown held the queue.
    manager.agent_queue.suspend(true).await;

    manager
        .restore_chat(&chat.id, std::process::id(), None, false)
        .await
        .unwrap_err();

    let saved = manager.chat_by_id(&chat.id).await.unwrap().unwrap();
    assert_eq!(saved.restore, Some(ChatRestore::Automatic));
}

#[tokio::test]
async fn a_saved_chat_keeps_its_files_listed() {
    let (root, manager) = manager().await;
    let (_repo, chat, mut process) = running_chat(root.path(), &manager).await;
    let notes = root.path().join("notes.md");
    std::fs::write(&notes, "kept").unwrap();
    manager
        .add_attached_files(
            &SpaceSelector::from(&chat.name),
            vec![notes.into()],
            &crate::daemon::workspace_files::Attacher::User,
        )
        .await
        .unwrap();
    manager.save_chats(ChatRestore::Resume).await;
    process.wait().unwrap();

    let files = manager.attached_files(&"talk".into()).await.unwrap();

    assert_eq!(files.len(), 1);
    assert_eq!(files[0].name, "notes.md");
}

#[tokio::test]
async fn no_chat_starts_while_a_shutdown_holds_the_queue() {
    let (root, manager) = manager().await;
    let checkout = repository(root.path(), "repo");
    let repo = manager
        .register_repository(checkout.to_str().unwrap().into(), None, None)
        .await
        .unwrap();
    manager.agent_queue.suspend(true).await;

    manager
        .start_chat(
            repo.id.into(),
            "late".into(),
            "claude".into(),
            std::process::id(),
            None,
            false,
        )
        .await
        .unwrap_err();

    assert!(manager.chats().await.unwrap().is_empty());
}

#[tokio::test]
async fn a_saved_chat_takes_no_handoff() {
    let (root, manager) = manager().await;
    let checkout = repository(root.path(), "repo");
    let repo = manager
        .register_repository(checkout.to_str().unwrap().into(), None, None)
        .await
        .unwrap();
    // A launcher that hands the chat to a session it starts, as Happy's does.
    let mut launcher = ProcessGroup::spawn(unmarked("sleep").arg("600"));
    let chat = manager
        .start_chat(
            repo.id.into(),
            "talk".into(),
            "happy-claude".into(),
            launcher.id(),
            None,
            true,
        )
        .await
        .unwrap();
    manager.save_chats(ChatRestore::Resume).await;
    launcher.wait().unwrap();

    manager
        .move_chat(&chat.id, std::process::id())
        .await
        .unwrap_err();

    let saved = manager.chat_by_id(&chat.id).await.unwrap().unwrap();
    assert_eq!(
        (saved.pid, saved.restore),
        (chat.pid, Some(ChatRestore::Resume))
    );
}

/// The child carries no chat identity: the lineage of the agent's
/// never-reused ID, which macOS reports, proves it the chat's.
#[cfg(target_os = "macos")]
#[tokio::test]
async fn a_stop_after_the_agent_exited_ends_what_it_left_running() {
    let (root, manager) = manager().await;
    let checkout = repository(root.path(), "repo");
    let repo = manager
        .register_repository(checkout.to_str().unwrap().into(), None, None)
        .await
        .unwrap();
    let started = root.path().join("child");
    let mut leader = ProcessGroup::spawn(
        unmarked("sh")
            .arg("-c")
            .arg("sleep 600 & echo $! > \"$0\"; wait")
            .arg(&started),
    );
    let group = KilledGroup(leader.id());
    let chat = manager
        .start_chat(
            repo.id.into(),
            "talk".into(),
            "claude".into(),
            leader.id(),
            None,
            false,
        )
        .await
        .unwrap();
    let child = wait_for_child(&started).await;
    let child = crate::process::identity::capture(child).unwrap().unwrap();
    let agent = crate::process::identity::capture(chat.pid)
        .unwrap()
        .unwrap();
    manager
        .store
        .run(|db| super::save(db, ChatRestore::Resume, None, None))
        .await
        .unwrap();
    // The agent exits between the save and its stop.
    unsafe { libc::kill(leader.id() as i32, libc::SIGKILL) };
    leader.wait().unwrap();

    manager.stop_saved(&chat.id, &agent).await.unwrap();

    assert!(!crate::process::identity::alive(&child).unwrap());
    drop(group);
}

#[tokio::test]
async fn a_stop_after_the_agent_exited_ends_what_carries_the_chat() {
    let (root, manager) = manager().await;
    let checkout = repository(root.path(), "repo");
    let repo = manager
        .register_repository(checkout.to_str().unwrap().into(), None, None)
        .await
        .unwrap();
    // The agent starts a child with the chat's identity once it is recorded.
    // The child writes its own PID, once it runs with that identity: before,
    // where only its environment finds it, as on Linux, no stop could.
    let id_file = root.path().join("chat-id");
    let started = root.path().join("child");
    let mut leader = ProcessGroup::spawn(
        unmarked("sh")
            .arg("-c")
            .arg(
                "while [ ! -s \"$0\" ]; do sleep 0.1; done; \
                 SHOAL_CHAT=$(cat \"$0\") sh -c 'echo $$ > \"$0\"; exec sleep 600' \"$1\" & wait",
            )
            .arg(&id_file)
            .arg(&started),
    );
    let group = KilledGroup(leader.id());
    let chat = manager
        .start_chat(
            repo.id.into(),
            "talk".into(),
            "claude".into(),
            leader.id(),
            None,
            false,
        )
        .await
        .unwrap();
    std::fs::write(&id_file, &chat.id).unwrap();
    let child = wait_for_child(&started).await;
    let child = crate::process::identity::capture(child).unwrap().unwrap();
    let agent = crate::process::identity::capture(chat.pid)
        .unwrap()
        .unwrap();
    manager
        .store
        .run(|db| super::save(db, ChatRestore::Resume, None, None))
        .await
        .unwrap();
    unsafe { libc::kill(leader.id() as i32, libc::SIGKILL) };
    leader.wait().unwrap();

    manager.stop_saved(&chat.id, &agent).await.unwrap();

    assert!(!crate::process::identity::alive(&child).unwrap());
    drop(group);
}

/// A stop signals a child of the agent and is interrupted, as by a daemon
/// crash; the child, terminating, starts a process of its own session
/// without the chat's identity and exits. Only the lineage of the child,
/// which the stop recorded before signalling it, still leads to that
/// process, by the never-reused IDs macOS reports.
#[cfg(target_os = "macos")]
#[tokio::test]
async fn an_interrupted_stop_leaves_what_its_signalled_child_started_traceable() {
    let (root, manager) = manager().await;
    let checkout = repository(root.path(), "repo");
    let repo = manager
        .register_repository(checkout.to_str().unwrap().into(), None, None)
        .await
        .unwrap();
    let started = root.path().join("child");
    let detached = root.path().join("descendant");
    // Terminating, the child starts a process in a session of its own and
    // exits once that process names itself, since macOS drops the link to an
    // exited parent when an adopted process replaces its image. The child
    // names itself once it handles the signal.
    let script = root.path().join("child.sh");
    std::fs::write(
        &script,
        r#"trap 'kill $s
perl -MPOSIX -e "POSIX::setsid(); open my \$f, q(>), shift; print \$f \$\$; close \$f; sleep 600" "$1" &
while [ ! -s "$1" ]; do sleep 0.1; done
exit' TERM
sleep 600 & s=$!
echo $$ > "$2"
wait
"#,
    )
    .unwrap();
    let mut leader = ProcessGroup::spawn(
        unmarked("sh")
            .arg("-c")
            .arg("sh \"$0\" \"$1\" \"$2\" & wait")
            .arg(&script)
            .arg(&detached)
            .arg(&started),
    );
    let group = KilledGroup(leader.id());
    let chat = manager
        .start_chat(
            repo.id.into(),
            "talk".into(),
            "claude".into(),
            leader.id(),
            None,
            false,
        )
        .await
        .unwrap();
    let child = wait_for_child(&started).await;
    let child = crate::process::identity::capture(child).unwrap().unwrap();
    let agent = crate::process::identity::capture(chat.pid)
        .unwrap()
        .unwrap();
    manager
        .store
        .run(|db| super::save(db, ChatRestore::Resume, None, None))
        .await
        .unwrap();
    // The stop records the child and signals it, then the daemon crashes.
    manager
        .record_chat_stopping(&chat.id, vec![child.clone()])
        .await
        .unwrap();
    unsafe { libc::kill(child.pid as i32, libc::SIGTERM) };
    let descendant = wait_for_child(&detached).await;
    let escaped = KilledGroup(descendant);
    while crate::process::identity::alive(&child).unwrap() {
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    }
    unsafe { libc::kill(leader.id() as i32, libc::SIGKILL) };
    leader.wait().unwrap();

    let error = manager.remove_chat(&"talk".into(), None).await.unwrap_err();
    assert!(
        error.to_string().contains(&descendant.to_string()),
        "{error:#}"
    );
    manager.stop_saved(&chat.id, &agent).await.unwrap();
    manager.remove_chat(&"talk".into(), None).await.unwrap();
    assert!(!chat.tmp_dir.unwrap().exists());
    drop(escaped);
    drop(group);
}

#[tokio::test]
async fn a_chat_a_removal_holds_is_not_restored() {
    let (root, manager) = manager().await;
    let (_repo, chat, _process) = running_chat(root.path(), &manager).await;
    manager.save_chats(ChatRestore::Resume).await;
    // As while a removal takes the workspaces below it.
    let reservation = manager.chat_removals.reserve(&chat.id).unwrap();
    assert!(manager.chat_removals.reserve(&chat.id).is_err());
    manager
        .restore_chat(&chat.id, std::process::id(), None, false)
        .await
        .unwrap_err();
    assert!(
        manager
            .chat_by_id(&chat.id)
            .await
            .unwrap()
            .unwrap()
            .restore
            .is_some()
    );
    // Once the removal gives up, the chat runs again.
    drop(reservation);
    let restored = manager
        .restore_chat(&chat.id, std::process::id(), None, false)
        .await
        .unwrap();
    assert!(restored.restore.is_none());
}

#[tokio::test]
async fn a_chat_a_removal_holds_takes_no_question() {
    let (root, manager) = manager().await;
    let (_repo, chat, _process) = running_chat(root.path(), &manager).await;
    let ask = || crate::daemon::questions::Ask {
        question: "Which database?".into(),
        recommendation: "SQLite".into(),
        reason: None,
        choices: Vec::new(),
    };
    let attacher = crate::daemon::workspace_files::Attacher::User;
    let reservation = manager.chat_removals.reserve(&chat.id).unwrap();
    manager
        .ask_question(
            &SpaceSelector::from(&chat.name),
            ask(),
            Vec::new(),
            &attacher,
        )
        .await
        .unwrap_err();
    assert!(manager.questions(None).await.unwrap().questions.is_empty());
    drop(reservation);
    manager
        .ask_question(
            &SpaceSelector::from(&chat.name),
            ask(),
            Vec::new(),
            &attacher,
        )
        .await
        .unwrap();
}

#[tokio::test]
async fn a_chat_a_removal_holds_neither_gives_up_nor_takes_workspaces() {
    let (root, manager) = manager().await;
    let (repo, chat, _process) = running_chat(root.path(), &manager).await;
    let below = manager
        .create_workspace_below(
            repo.id.clone().into(),
            "below".into(),
            None,
            Default::default(),
            Some(chat.id.clone()),
        )
        .await
        .unwrap();
    let other = manager
        .create_workspace(&repo.id, "other".into(), None, None, None)
        .await
        .unwrap();
    let reservation = manager.chat_removals.reserve(&chat.id).unwrap();
    // The workspace the removal takes stays below the chat, opened outside
    // it or from it again...
    assert!(manager.assign_parent(&below.id, None).await.is_err());
    manager
        .assign_parent(&below.id, Some(chat.id.clone()))
        .await
        .unwrap();
    assert_eq!(
        manager
            .workspace_by_id(&below.id)
            .await
            .unwrap()
            .parent
            .map(|parent| parent.id),
        Some(chat.id.clone())
    );
    // ...and one opened from it meanwhile is the user's.
    manager
        .assign_parent(&other.id, Some(chat.id.clone()))
        .await
        .unwrap();
    assert_eq!(
        manager
            .workspace_by_id(&other.id)
            .await
            .unwrap()
            .parent
            .map(|parent| parent.id),
        None
    );
    drop(reservation);
    manager.assign_parent(&below.id, None).await.unwrap();
}

/// [`a_survivor_that_left_the_chats_group_holds_it_saved`] for an agent that
/// exited on its own, which no save saw.
#[cfg(target_os = "macos")]
#[tokio::test]
async fn a_survivor_that_left_the_group_of_an_exited_agent_holds_the_chat() {
    let (root, manager) = manager().await;
    let checkout = repository(root.path(), "repo");
    let repo = manager
        .register_repository(checkout.to_str().unwrap().into(), None, None)
        .await
        .unwrap();
    let started = root.path().join("child");
    let mut leader = ProcessGroup::spawn(
        unmarked("sh")
            .arg("-c")
            .arg("perl -MPOSIX -e 'POSIX::setsid(); sleep 600' & echo $! > \"$0\"; wait")
            .arg(&started),
    );
    let chat = manager
        .start_chat(
            repo.id.clone().into(),
            "talk".into(),
            "claude".into(),
            leader.id(),
            None,
            false,
        )
        .await
        .unwrap();
    let child = wait_for_child(&started).await;
    let escaped = KilledGroup(child);
    while unsafe { libc::getpgid(child as i32) } != child as i32 {
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    }
    // The agent exits on its own, which no save saw.
    unsafe { libc::kill(leader.id() as i32, libc::SIGKILL) };
    leader.wait().unwrap();
    manager.stop_finished_chats().await.unwrap();
    assert!(
        manager
            .chat(&"talk".into())
            .await
            .unwrap()
            .restore
            .is_some()
    );

    let error = manager.remove_chat(&"talk".into(), None).await.unwrap_err();
    assert!(error.to_string().contains(&child.to_string()), "{error:#}");
    assert!(chat.tmp_dir.unwrap().exists());
    drop(escaped);
}
