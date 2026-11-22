use crate::{
    daemon::{
        agent_service::tests::queue::{add, admitted, register},
        manager::Manager,
        test_support::{global_config, manager},
    },
    model::Repository,
    state::QueueState,
};
use shoal_ids::queue_entry_ids::QueueEntryId;
use shoal_protocol::{
    launches::{Launch, LaunchSource, StartTarget},
    protocol::Started,
};

/// Whether the daemon started the queued `id` itself, wherever it runs.
fn started_launch(id: &QueueEntryId) -> bool {
    let started = crate::daemon::queued_starts::fake::STARTED.lock().unwrap();
    started.iter().any(|(started, _)| started == id)
}

/// A machine of one heavy slot with a heavy chat mode `pair` and a light
/// one `chat`, and the workspace launch `busy` holding the slot. Its
/// `claude` is a program every runner has, since a chat's launch checks
/// that its agent is installed.
async fn full_machine(manager: &Manager, repo: &Repository) {
    manager.publish_config(global_config(
        "[machine]\nmax_heavy_agents = 1\n[machine.overload.disk]\nenabled = false\n\
         [modes.pair]\nkind = 'chat'\n[modes.solo]\nkind = 'chat'\nrepository = false\n\
         [agents.claude]\ncommand = ['sh']\n",
    ));
    assert!(admitted(&add(manager, repo, "busy").await));
}

/// The launch of the chat `name` of `repo` in the heavy mode `pair`,
/// started `now` when asked.
fn heavy_chat(repo: &Repository, name: &str, now: bool) -> Launch {
    Launch {
        target: Some(StartTarget::Repository(repo.id.to_string())),
        mode: Some("pair".into()),
        name: Some(name.into()),
        agent: Some("claude".into()),
        now,
        ..Launch::default()
    }
}

/// A launched chat whose heavy agent must wait is recorded at once, and
/// the answer names the chat and its agent's entry, which waits there.
#[tokio::test]
async fn a_launched_chat_whose_agent_waits_is_recorded_with_its_entry() {
    let (temp, manager) = manager().await;
    let repo = register(&manager, &temp, "repo").await;
    full_machine(&manager, &repo).await;

    let launch = heavy_chat(&repo, "queued talk", false);
    let started = (manager.launch(launch, LaunchSource::Manual, None))
        .await
        .unwrap();

    let Started::Queued { waiting } = started.start else {
        panic!("expected a queued chat: {started:?}");
    };
    assert_eq!(waiting, 1);
    let queued = started.entry.expect("the answer names its agent's entry");
    assert_eq!(queued.state, QueueState::Queued);
    assert_eq!(queued.space(), Some(&started.space.id));
    let chats = manager.chats().await.unwrap();
    let chat = chats.iter().find(|chat| chat.id == started.space.id);
    assert_eq!(chat.unwrap().name, "queued talk");
    assert!(!started_launch(&queued.id));
}

/// A launched chat asked to start now has its agent start past the full
/// machine, still taking a slot.
#[tokio::test]
async fn a_launched_chat_started_now_runs_past_the_limits() {
    let (temp, manager) = manager().await;
    let repo = register(&manager, &temp, "repo").await;
    full_machine(&manager, &repo).await;

    let launch = heavy_chat(&repo, "urgent talk", true);
    let started = (manager.launch(launch, LaunchSource::Manual, None))
        .await
        .unwrap();

    let entry = started.entry.unwrap();
    assert_eq!(entry.state, QueueState::Starting);
    assert!(started_launch(&entry.id));
}

/// A recorded chat's agent queues through the same request as a
/// workspace's, waiting for a slot as one does, and a second launch there
/// is refused while it waits.
#[tokio::test]
async fn a_recorded_chats_agent_queues_as_a_workspaces_does() {
    let (temp, manager) = manager().await;
    let repo = register(&manager, &temp, "repo").await;
    full_machine(&manager, &repo).await;
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
    let launch = crate::protocol::operations::QueuedLaunch {
        mode: Some("pair".into()),
        ..crate::protocol::operations::QueuedLaunch::new("claude".into())
    };
    let request = crate::start_plan::queued_agent_request(&chat, launch, None).unwrap();

    let admission = manager
        .agents
        .queue_add(&*manager, request.clone())
        .await
        .unwrap();

    let entry = admission.entry.unwrap();
    assert_eq!(entry.state, QueueState::Queued);
    assert_eq!(entry.space(), Some(&chat.id));
    assert!(manager.agents.queue_add(&*manager, request).await.is_err());
}
