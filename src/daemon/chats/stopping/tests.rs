use std::{
    os::unix::process::CommandExt as _,
    sync::{
        Mutex,
        atomic::{AtomicUsize, Ordering},
    },
};

use crate::{
    daemon::chats::saved::tests::running_chat, process::identity::Identity, test_support::manager,
};

/// The agent starts a process in its group as its stop's first round
/// signals it, so a second round stops that process after the agent exited.
/// A listing and a sweep in between find the agent exited.
#[tokio::test]
async fn a_chat_keeps_its_record_and_directory_until_its_stop_finishes() {
    let (root, manager) = manager().await;
    // Its group's end at the test's end takes what the agent started.
    let (_repo, chat, _agent) = running_chat(root.path(), &manager).await;
    let tmp_dir = chat.tmp_dir.clone().unwrap();
    let rounds = AtomicUsize::new(0);
    let started = Mutex::new(Vec::new());
    let between_rounds = Mutex::new(None);

    manager
        .end_chat_recording(&chat.id, |found: Vec<Identity>| {
            let (manager, chat, tmp_dir) = (&manager, &chat, &tmp_dir);
            let (rounds, started, between_rounds) = (&rounds, &started, &between_rounds);
            async move {
                manager.record_chat_stopping(&chat.id, found).await?;
                if rounds.fetch_add(1, Ordering::SeqCst) == 0 {
                    let child = std::process::Command::new("sleep")
                        .arg("600")
                        .process_group(i32::try_from(chat.pid)?)
                        .spawn()?;
                    started.lock().unwrap().push(child);
                } else {
                    let listed = manager.recorded_chat(&chat.id).await?.is_some();
                    manager.end_finished_chats().await?;
                    let swept = manager.recorded_chat(&chat.id).await?.is_some();
                    *between_rounds.lock().unwrap() = Some((listed, swept, tmp_dir.exists()));
                }
                Ok(())
            }
        })
        .await
        .unwrap();

    assert_eq!(rounds.load(Ordering::SeqCst), 2);
    assert_eq!(*between_rounds.lock().unwrap(), Some((true, true, true)));
    assert!(manager.recorded_chat(&chat.id).await.unwrap().is_none());
    assert!(!tmp_dir.exists());
    for mut child in started.into_inner().unwrap() {
        child.wait().unwrap();
    }
}
