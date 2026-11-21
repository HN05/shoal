use shoal_core::config::Config;

use super::*;
use crate::stand_in;

fn recorded(sent: Option<&str>, tried: Option<UnixTime>) -> Recorded {
    Recorded {
        sent: sent.map(str::to_owned),
        tried_at: tried,
        unsent_since: None,
    }
}

#[test]
fn a_space_needs_a_package_only_once_it_changed_and_the_last_try_is_old_enough() {
    let now = UnixTime::now();
    let state = || "b".to_owned();
    assert_eq!(
        look(&recorded(Some("b"), Some(now)), state(), now),
        Look::Current
    );
    assert_eq!(
        look(&recorded(None, None), state(), now),
        Look::Send(state())
    );
    let lately = now - Duration::from_secs(60);
    assert_eq!(
        look(&recorded(Some("a"), Some(lately)), state(), now),
        Look::Wait(SEND_EVERY - Duration::from_secs(60))
    );
    let long_ago = now - SEND_EVERY;
    assert_eq!(
        look(&recorded(Some("a"), Some(long_ago)), state(), now),
        Look::Send(state())
    );
}

/// What the machine recorded of the packages of the space `id`.
async fn recorded_of(hub: &Hub, id: &SpaceId) -> Recorded {
    let id = id.clone();
    hub.store.read(move |tx| load(tx, &id)).await.unwrap()
}

#[tokio::test]
async fn an_unchanged_space_needs_no_package_and_a_change_records_since_when() {
    let root = tempfile::tempdir().unwrap();
    let hub = stand_in::hub(&root, Config::from_toml("").unwrap()).await;
    let id = SpaceId::from("plan".to_owned());
    let first = UnixTime::now();
    let look = hub.record_look(&id, "a".into(), first).await.unwrap();
    assert_eq!(look, Look::Send("a".into()));
    assert_eq!(recorded_of(&hub, &id).await.unsent_since, Some(first));
    let sent = id.clone();
    (hub.store)
        .write(move |tx| record_sent(tx, &sent, "a"))
        .await
        .unwrap();
    let look = hub.record_look(&id, "a".into(), first).await.unwrap();
    assert_eq!(look, Look::Current);
    assert_eq!(recorded_of(&hub, &id).await.unsent_since, None);
    let later = first + Duration::from_secs(1);
    let look = hub.record_look(&id, "b".into(), later).await.unwrap();
    assert_eq!(look, Look::Send("b".into()));
    assert_eq!(recorded_of(&hub, &id).await.unsent_since, Some(later));
}

#[tokio::test]
async fn only_an_intermittent_machine_reports_since_when_a_package_lacks_changes() {
    use shoal_protocol::hub::availability::{AvailabilityReason, Classified};
    let root = tempfile::tempdir().unwrap();
    let hub = stand_in::hub(&root, Config::from_toml("").unwrap()).await;
    let id = SpaceId::from("plan".to_owned());
    let now = UnixTime::now();
    hub.record_look(&id, "a".into(), now).await.unwrap();
    assert!(hub.unsent_changes().await.unwrap().is_empty());
    hub.member.told(Classified {
        availability: Availability::Intermittent,
        reason: AvailabilityReason::Set,
    });
    let unsent = hub.unsent_changes().await.unwrap();
    assert_eq!(unsent.get(&id), Some(&now));
}

#[test]
fn a_pass_over_ended_turns_keeps_an_earlier_held_back_send() {
    let now = Instant::now();
    let (soon, later) = (now + Duration::from_secs(60), now + SEND_EVERY);
    assert_eq!(next_due(Some(soon), None, false), Some(soon));
    assert_eq!(next_due(Some(later), Some(soon), false), Some(soon));
    assert_eq!(next_due(Some(soon), Some(later), false), Some(soon));
    // A pass over every space finds every send held back.
    assert_eq!(next_due(Some(soon), Some(later), true), Some(later));
    assert_eq!(next_due(Some(soon), None, true), None);
}
