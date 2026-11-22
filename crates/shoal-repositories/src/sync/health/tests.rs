use std::time::Duration;

use anyhow::anyhow;
use tokio::time::Instant;

use super::{Health, NEEDS_USER_AFTER, UnixTime};

#[test]
fn a_failure_needs_the_user_once_it_lasts_and_is_told_once() {
    let start = Instant::now();
    let mut health = Health::default();
    health.record(None, start, UnixTime::from_seconds(100));
    health.record(Some(&anyhow!("first")), start, UnixTime::from_seconds(160));
    let later = start + NEEDS_USER_AFTER - Duration::from_secs(1);
    health.record(
        Some(&anyhow!("diverged")),
        later,
        UnixTime::from_seconds(2000),
    );
    assert!(!health.needs_user(later));
    assert_eq!(health.notify(later), None);

    let lasted = start + NEEDS_USER_AFTER;
    assert!(health.needs_user(lasted));
    assert_eq!(health.notify(lasted).as_deref(), Some("diverged"));
    assert_eq!(health.notify(lasted), None);
    let view = health.view(&"repo".into()).unwrap();
    assert_eq!(view.synced_at, Some(UnixTime::from_seconds(100)));
    assert_eq!(view.failing_since, Some(UnixTime::from_seconds(160)));
    assert_eq!(view.error.as_deref(), Some("diverged"));
}

#[test]
fn a_fetch_alone_shows_nothing_until_the_sync_finishes() {
    let start = Instant::now();
    let mut health = Health::default();
    health.fetched(start);
    assert_eq!(health.view(&"repo".into()), None);
    health.record(None, start, UnixTime::from_seconds(100));
    assert_eq!(
        health.view(&"repo".into()).unwrap().synced_at,
        Some(UnixTime::from_seconds(100))
    );
}

#[test]
fn a_successful_sync_ends_the_failure() {
    let start = Instant::now();
    let mut health = Health::default();
    health.record(
        Some(&anyhow!("offline")),
        start,
        UnixTime::from_seconds(100),
    );
    let lasted = start + NEEDS_USER_AFTER;
    health.record(None, lasted, UnixTime::from_seconds(2000));
    assert!(!health.needs_user(lasted));
    let view = health.view(&"repo".into()).unwrap();
    assert_eq!(view.synced_at, Some(UnixTime::from_seconds(2000)));
    assert_eq!((view.failing_since, view.error), (None, None));
}
