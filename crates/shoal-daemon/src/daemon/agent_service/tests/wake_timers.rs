use std::time::Duration;

use shoal_protocol::{assignments::Done, sleep::WakeTimer, space_names::SpaceSelector};

use crate::{
    daemon::{
        manager::Manager,
        test_support::{manager, repository},
        workspace_files::Attacher,
    },
    model::Repository,
    state::Sleep,
    time::UnixTime,
};

const MINUTE: Duration = Duration::from_secs(60);

async fn registered(manager: &Manager, temp: &tempfile::TempDir) -> Repository {
    let checkout = repository(temp.path(), "repo");
    manager
        .repositories
        .register(checkout.to_str().unwrap().into(), None, None)
        .await
        .unwrap()
}

async fn workspace(manager: &Manager, repo: &Repository, name: &str) -> SpaceSelector {
    let workspace = manager
        .create_workspace(&repo.id, name.into(), None, None, None)
        .await
        .unwrap();
    SpaceSelector::id(&workspace.id)
}

/// A timer `minutes` from now with `message`.
fn timer(minutes: u32, message: Option<&str>) -> WakeTimer {
    WakeTimer {
        at: UnixTime::now() + MINUTE * minutes,
        message: message.map(str::to_owned),
    }
}

/// The unread messages of the space `selector` names.
async fn messages(manager: &Manager, selector: &SpaceSelector) -> Vec<String> {
    (manager
        .agents
        .agent_messages(manager, selector)
        .await
        .unwrap())
    .into_iter()
    .map(|message| message.message)
    .collect()
}

/// Fire the timers due `minutes` from now.
async fn fire_in(manager: &Manager, minutes: u32) -> Option<UnixTime> {
    let later = UnixTime::now() + MINUTE * minutes;
    manager.agents.fire_wake_timers(later).await.unwrap()
}

#[tokio::test]
async fn waking_the_agent_and_finishing_the_work_cancel_its_timer() {
    let (temp, manager) = manager().await;
    let repo = registered(&manager, &temp).await;
    let woken = workspace(&manager, &repo, "woken").await;
    let done = workspace(&manager, &repo, "done").await;
    for selector in [&woken, &done] {
        (manager.agents.sleep(
            &*manager,
            selector,
            Some(Sleep::Turn),
            Some(timer(20, None)),
        ))
        .await
        .unwrap();
    }
    let undone = manager
        .agents
        .sleep(&*manager, &woken, None, None)
        .await
        .unwrap();
    assert_eq!(undone.wake_at, None);
    (manager
        .spaces
        .mark_space_done(&*manager, &done, Done::default(), &Attacher::User))
    .await
    .unwrap();
    assert_eq!(fire_in(&manager, 30).await, None);
    for selector in [&woken, &done] {
        assert!(messages(&manager, selector).await.is_empty());
    }
    // Waking the agent sets no timer.
    let refused = manager
        .agents
        .sleep(&*manager, &woken, None, Some(timer(20, None)))
        .await;
    assert!(refused.is_err());
}
