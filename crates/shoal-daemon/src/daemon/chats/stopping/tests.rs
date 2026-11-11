use std::{
    os::unix::process::CommandExt as _,
    sync::{
        Mutex,
        atomic::{AtomicUsize, Ordering},
    },
};

use crate::{
    daemon::{
        chats::saved::tests::running_chat,
        test_support::{manager, unmarked},
    },
    process::identity::Identity,
};

/// The agent starts a process in its group as its stop's first round
/// signals it, so a second round stops that process after the agent exited.
/// A listing and a sweep in between find the agent exited.
#[tokio::test]
async fn a_chat_runs_until_its_stop_finishes() {
    let (root, manager) = manager().await;
    // Its group's end at the test's end takes what the agent started.
    let (_repo, chat, _agent) = running_chat(root.path(), &manager).await;
    let tmp_dir = chat.tmp_dir.clone().unwrap();
    let rounds = AtomicUsize::new(0);
    let started = Mutex::new(Vec::new());
    let between_rounds = Mutex::new(None);

    manager
        .stop_chat_recording(&chat.id, |found: Vec<Identity>| {
            let (manager, chat, tmp_dir) = (&manager, &chat, &tmp_dir);
            let (rounds, started, between_rounds) = (&rounds, &started, &between_rounds);
            async move {
                manager.record_chat_stopping(&chat.id, found).await?;
                if rounds.fetch_add(1, Ordering::SeqCst) == 0 {
                    let child = unmarked("sleep")
                        .arg("600")
                        .process_group(i32::try_from(chat.pid)?)
                        .spawn()?;
                    started.lock().unwrap().push(child);
                } else {
                    let running = |listed: Option<crate::model::Chat>| {
                        listed.is_some_and(|chat| chat.restore.is_none())
                    };
                    let listed = running(manager.chat_by_id(&chat.id).await?);
                    manager.stop_finished_chats().await?;
                    let swept = running(manager.chat_by_id(&chat.id).await?);
                    *between_rounds.lock().unwrap() = Some((listed, swept, tmp_dir.exists()));
                }
                Ok(())
            }
        })
        .await
        .unwrap();

    assert_eq!(rounds.load(Ordering::SeqCst), 2);
    assert_eq!(*between_rounds.lock().unwrap(), Some((true, true, true)));
    let stopped = manager.chat_by_id(&chat.id).await.unwrap().unwrap();
    assert_eq!(stopped.restore, Some(crate::state::ChatRestore::Resume));
    assert!(tmp_dir.exists());
    for mut child in started.into_inner().unwrap() {
        child.wait().unwrap();
    }
}
