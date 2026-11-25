use super::*;
use crate::{stand_in::Fixture, state::Sleep};
use shoal_protocol::space_names::SpaceSelector;

const MINUTE: Duration = Duration::from_secs(60);

/// A recorded workspace `name`, as a selector names it.
async fn workspace(fixture: &Fixture, name: &str) -> SpaceSelector {
    SpaceSelector::id(&fixture.workspace(name).await.id)
}

/// When the timer of the space `selector` names fires.
async fn wake_at(fixture: &Fixture, selector: &SpaceSelector) -> Option<UnixTime> {
    let id = selector.given_id().unwrap().clone();
    (fixture.agents.store)
        .read(move |db| due_at(db, &id))
        .await
        .unwrap()
}

/// A timer `minutes` from now with `message`.
fn timer(minutes: u32, message: Option<&str>) -> WakeTimer {
    WakeTimer {
        at: UnixTime::now() + MINUTE * minutes,
        message: message.map(str::to_owned),
    }
}

/// The unread messages of the space `selector` names.
async fn messages(fixture: &Fixture, selector: &SpaceSelector) -> Vec<String> {
    (fixture
        .agents
        .agent_messages(&*fixture.daemon, selector)
        .await
        .unwrap())
    .into_iter()
    .map(|message| message.message)
    .collect()
}

/// Fire the timers due `minutes` from now.
async fn fire_in(fixture: &Fixture, minutes: u32) -> Option<UnixTime> {
    let later = UnixTime::now() + MINUTE * minutes;
    fixture.agents.fire_wake_timers(later).await.unwrap()
}

#[tokio::test]
async fn a_timer_sends_its_message_once_due_and_a_new_one_replaces_it() {
    let fixture = Fixture::new().await;
    let selector = workspace(&fixture, "waits").await;
    let first = timer(20, Some("Check whether CI finished"));
    (fixture
        .agents
        .sleep(&*fixture.daemon, &selector, Some(Sleep::Turn), Some(first)))
    .await
    .unwrap();
    let second = timer(40, Some("Check the review"));
    let slept = (fixture.agents.sleep(
        &*fixture.daemon,
        &selector,
        Some(Sleep::Turn),
        Some(second.clone()),
    ))
    .await
    .unwrap();
    assert_eq!(slept.wake_at, Some(second.at));
    assert_eq!(wake_at(&fixture, &selector).await, Some(second.at));
    // A plain sleep keeps the timer.
    let slept = (fixture
        .agents
        .sleep(&*fixture.daemon, &selector, Some(Sleep::Permanent), None))
    .await
    .unwrap();
    assert_eq!(slept.wake_at, Some(second.at));

    assert_eq!(fire_in(&fixture, 30).await, Some(second.at));
    assert!(messages(&fixture, &selector).await.is_empty());
    assert_eq!(fire_in(&fixture, 41).await, None);
    let delivered = messages(&fixture, &selector).await;
    assert_eq!(delivered.len(), 1, "{delivered:?}");
    assert!(
        delivered[0].ends_with(": Check the review"),
        "{delivered:?}"
    );
    assert_eq!(wake_at(&fixture, &selector).await, None);
    fire_in(&fixture, 60).await;
    assert_eq!(messages(&fixture, &selector).await.len(), 1);
}

#[tokio::test]
async fn a_failed_firing_holds_back_only_the_timer_that_failed() {
    let fixture = Fixture::new().await;
    let failed = workspace(&fixture, "failed").await;
    let replaced = workspace(&fixture, "replaced").await;
    let now = UnixTime::now();
    let replacement = timer(120, None);
    for (selector, timer) in [
        (
            &failed,
            WakeTimer {
                at: now,
                message: None,
            },
        ),
        (&replaced, replacement.clone()),
    ] {
        (fixture
            .agents
            .sleep(&*fixture.daemon, selector, Some(Sleep::Turn), Some(timer)))
        .await
        .unwrap();
    }
    let later = now + MINUTE;
    for selector in [&failed, &replaced] {
        let id = selector.given_id().unwrap().clone();
        (fixture
            .agents
            .store
            .write(move |tx| postpone(tx, &id, now, later)))
        .await
        .unwrap();
    }
    assert_eq!(wake_at(&fixture, &failed).await, Some(later));
    // A timer set after the firing failed keeps its own time.
    let kept = wake_at(&fixture, &replaced).await;
    assert_eq!(kept, Some(replacement.at));
}

#[tokio::test]
async fn a_chats_timer_without_a_message_says_it_fired() {
    let fixture = Fixture::new().await;
    let repo = fixture.repository().await;
    let talks = fixture.chat_in("talks", "chat", Some(&repo)).await;
    let selector = SpaceSelector::id(&talks);
    let sleep = Some(Sleep::Turn);
    (fixture
        .agents
        .sleep(&*fixture.daemon, &selector, sleep, Some(timer(5, None))))
    .await
    .unwrap();
    fire_in(&fixture, 5).await;
    let delivered = messages(&fixture, &selector).await;
    assert_eq!(delivered.len(), 1, "{delivered:?}");
    assert!(delivered[0].ends_with("fired."), "{delivered:?}");
}
