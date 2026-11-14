use std::collections::BTreeMap;

use crate::client::{output::chat::StartedChat, space_names::SpaceSelector};
use crate::{
    daemon::{
        test_support::{global_config, manager},
        workspace::{
            Manager,
            agent_queue::tests::{add, register, started},
        },
    },
    model::{ChatQueueRequest, Repository},
    protocol::ChatStart,
    state::QueueState,
};

/// A machine of one agent slot with a heavy chat mode `pair` and a light
/// one `chat`, and the workspace launch `busy` holding the slot.
async fn full_machine(manager: &Manager, repo: &Repository) {
    manager.publish_config(global_config(
        "[machine]\nmax_agents = 1\n[machine.overload.disk]\nenabled = false\n\
         [modes.pair]\nkind = 'chat'\n[modes.solo]\nkind = 'chat'\nrepository = false\n",
    ));
    assert!(add(manager, repo, "busy").await.start);
}

/// The launch of a chat's agent in `mode`, of `repository` or of none.
fn chat_launch(repository: Option<&Repository>, mode: &str) -> ChatQueueRequest {
    ChatQueueRequest {
        repository: repository.map(|repository| (&repository.id).into()),
        name: "talk".into(),
        agent: "claude".into(),
        mode: mode.into(),
        narrowing: Default::default(),
        plan: "{}".into(),
        env: BTreeMap::from([("HERDR_ENV".into(), "1".into())]),
        now: false,
        priority: None,
        records_when_waiting: false,
    }
}

/// A heavy chat's launch waits for a slot, as a heavy workspace agent's
/// does, and a light one's starts beside the full machine, of a chat with a
/// repository or without one.
#[tokio::test]
async fn a_chats_launch_waits_for_a_slot_by_its_modes_weight() {
    let (temp, manager) = manager().await;
    let repo = register(&manager, &temp, "repo").await;
    full_machine(&manager, &repo).await;

    for (repository, mode) in [(Some(&repo), "pair"), (None, "solo")] {
        let admission = manager.queue_chat_add(chat_launch(repository, mode), None);
        let admission = admission.await.unwrap();
        let entry = admission.entry.unwrap();
        assert!(
            !admission.start && entry.state == QueueState::Queued,
            "{mode}"
        );
    }
    let light = manager.queue_chat_add(chat_launch(Some(&repo), "chat"), None);
    let light = light.await.unwrap();
    assert!(light.start);
    assert_eq!(light.entry.unwrap().state, QueueState::Starting);
}

/// A waiting chat launch whose caller records its chat stays out of
/// admission until it takes the chat, even with a slot free, and then
/// starts from the queue in that chat, in the place the queuing terminal
/// names.
#[tokio::test]
async fn a_waiting_chat_launch_starts_only_in_its_recorded_chat() {
    let (temp, manager) = manager().await;
    let repo = register(&manager, &temp, "repo").await;
    full_machine(&manager, &repo).await;
    let launch = ChatQueueRequest {
        records_when_waiting: true,
        ..chat_launch(Some(&repo), "pair")
    };
    let entry = manager.queue_chat_add(launch, None).await;
    let entry = entry.unwrap().entry.unwrap();
    let busy = manager.queue_entries().await.unwrap();
    let busy = busy.iter().find(|queued| queued.name == "busy").unwrap();
    manager.fail_queue_entry(&busy.id, "gone").await.unwrap();
    manager.dispatch_queue().await.unwrap();
    assert!(!started(&entry.id), "its chat is not attached yet");
    let chat = crate::daemon::chats::NewChat {
        name: "talk".into(),
        agent: "claude".into(),
        mode: "pair".into(),
        narrowing: Default::default(),
        origin: None,
        parent: None,
        priority: None,
    };
    let chat = manager.start_new_chat(Some((&repo.id).into()), None, chat);
    let chat = chat.await.unwrap();

    (manager.queue_attach(&entry.id, &SpaceSelector::id(&chat.id)))
        .await
        .unwrap();
    manager.dispatch_queue().await.unwrap();

    assert!(started(&entry.id));
    let entries = manager.queue_entries().await.unwrap();
    let starting = entries.iter().find(|queued| queued.id == entry.id).unwrap();
    assert_eq!(starting.space_id.as_ref(), Some(&chat.id));
}

/// A client's chat whose heavy agent must wait is recorded at once, and
/// the answer names the chat and its agent's entry, without running the
/// chat's start worker.
#[tokio::test]
async fn a_clients_chat_whose_agent_waits_is_recorded_with_its_entry() {
    let (temp, manager) = manager().await;
    let repo = register(&manager, &temp, "repo").await;
    full_machine(&manager, &repo).await;
    let start = ChatStart {
        repository: Some((&repo.id).into()),
        agent: Some("claude".into()),
        mode: Some("pair".into()),
        name: Some("queued talk".into()),
        ..Default::default()
    };

    let started = manager.launch_chat(start, None).await.unwrap();

    let StartedChat::Queued(queued) = started else {
        panic!("expected a queued chat: {started:?}");
    };
    assert_eq!(queued.entry.state, QueueState::Queued);
    let entries = manager.queue_entries().await.unwrap();
    let entry = entries.iter().find(|entry| entry.id == queued.entry.id);
    assert_eq!(entry.unwrap().space_id.as_ref(), Some(&queued.chat.id));
    let launched = crate::daemon::chats::launches::tests::STARTED.lock();
    let names: Vec<_> = (launched.unwrap().iter())
        .filter_map(|(start, _)| start.name.clone())
        .collect();
    assert!(!names.contains(&"queued talk".into()), "{names:?}");
}
