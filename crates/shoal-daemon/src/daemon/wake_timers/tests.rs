use super::*;
use crate::{
    config::modes::ModeName,
    daemon::{
        test_support::{chat, manager, repository},
        workspace_files::Attacher,
    },
    model::Repository,
    state::Sleep,
};
use shoal_protocol::{assignments::Done, space_names::SpaceSelector};

const MINUTE: Duration = Duration::from_secs(60);

async fn registered(manager: &Manager, temp: &tempfile::TempDir) -> Repository {
    let checkout = repository(temp.path(), "repo");
    manager
        .register_repository(checkout.to_str().unwrap().into(), None, None)
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
    (manager.agent_messages(selector).await.unwrap())
        .into_iter()
        .map(|message| message.message)
        .collect()
}

/// Fire the timers due `minutes` from now.
async fn fire_in(manager: &Manager, minutes: u32) -> Option<UnixTime> {
    let later = UnixTime::now() + MINUTE * minutes;
    manager.fire_wake_timers(later).await.unwrap()
}

#[tokio::test]
async fn a_timer_sends_its_message_once_due_and_a_new_one_replaces_it() {
    let (temp, manager) = manager().await;
    let repo = registered(&manager, &temp).await;
    let selector = workspace(&manager, &repo, "waits").await;
    let first = timer(20, Some("Check whether CI finished"));
    (manager.sleep(&selector, Some(Sleep::Turn), Some(first)))
        .await
        .unwrap();
    let second = timer(40, Some("Check the review"));
    let slept = (manager.sleep(&selector, Some(Sleep::Turn), Some(second.clone())))
        .await
        .unwrap();
    assert_eq!(slept.wake_at, Some(second.at));
    let space = manager.space(&selector).await.unwrap();
    assert_eq!(space.wake_at, Some(second.at));
    // A plain sleep keeps the timer.
    let slept = (manager.sleep(&selector, Some(Sleep::Permanent), None))
        .await
        .unwrap();
    assert_eq!(slept.wake_at, Some(second.at));

    assert_eq!(fire_in(&manager, 30).await, Some(second.at));
    assert!(messages(&manager, &selector).await.is_empty());
    assert_eq!(fire_in(&manager, 41).await, None);
    let delivered = messages(&manager, &selector).await;
    assert_eq!(delivered.len(), 1, "{delivered:?}");
    assert!(
        delivered[0].ends_with(": Check the review"),
        "{delivered:?}"
    );
    assert_eq!(manager.space(&selector).await.unwrap().wake_at, None);
    fire_in(&manager, 60).await;
    assert_eq!(messages(&manager, &selector).await.len(), 1);
}

#[tokio::test]
async fn waking_the_agent_and_finishing_the_work_cancel_its_timer() {
    let (temp, manager) = manager().await;
    let repo = registered(&manager, &temp).await;
    let woken = workspace(&manager, &repo, "woken").await;
    let done = workspace(&manager, &repo, "done").await;
    for selector in [&woken, &done] {
        (manager.sleep(selector, Some(Sleep::Turn), Some(timer(20, None))))
            .await
            .unwrap();
    }
    let undone = manager.sleep(&woken, None, None).await.unwrap();
    assert_eq!(undone.wake_at, None);
    (manager.mark_space_done(&done, Done::default(), &Attacher::User))
        .await
        .unwrap();
    assert_eq!(fire_in(&manager, 30).await, None);
    for selector in [&woken, &done] {
        assert!(messages(&manager, selector).await.is_empty());
    }
    // Waking the agent sets no timer.
    let refused = manager.sleep(&woken, None, Some(timer(20, None))).await;
    assert!(refused.is_err());
}

#[tokio::test]
async fn a_chats_timer_without_a_message_says_it_fired() {
    let (temp, manager) = manager().await;
    let repo = registered(&manager, &temp).await;
    let talks = chat(&manager, "talks", ModeName::CHAT, Some(&repo), None).await;
    let selector = SpaceSelector::id(&talks.id);
    (manager.sleep(&selector, Some(Sleep::Turn), Some(timer(5, None))))
        .await
        .unwrap();
    fire_in(&manager, 5).await;
    assert_eq!(messages(&manager, &selector).await, [delivery(None)]);
}
