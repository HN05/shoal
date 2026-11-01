use std::time::Duration;

use anyhow::anyhow;
use tokio::time::Instant;

use super::{Health, NEEDS_USER_AFTER};

#[test]
fn a_failure_needs_the_user_once_it_lasts_and_is_told_once() {
    let start = Instant::now();
    let mut health = Health::default();
    health.record(None, start, 100);
    health.record(Some(&anyhow!("first")), start, 160);
    let later = start + NEEDS_USER_AFTER - Duration::from_secs(1);
    health.record(Some(&anyhow!("diverged")), later, 2000);
    assert!(!health.needs_user(later));
    assert_eq!(health.notify(later), None);

    let lasted = start + NEEDS_USER_AFTER;
    assert!(health.needs_user(lasted));
    assert_eq!(health.notify(lasted).as_deref(), Some("diverged"));
    assert_eq!(health.notify(lasted), None);
    let view = health.view("repo");
    assert_eq!(view.synced_at, Some(100));
    assert_eq!(view.failing_since, Some(160));
    assert_eq!(view.error.as_deref(), Some("diverged"));
}

#[test]
fn a_successful_sync_ends_the_failure() {
    let start = Instant::now();
    let mut health = Health::default();
    health.record(Some(&anyhow!("offline")), start, 100);
    let lasted = start + NEEDS_USER_AFTER;
    health.record(None, lasted, 2000);
    assert!(!health.needs_user(lasted));
    let view = health.view("repo");
    assert_eq!(view.synced_at, Some(2000));
    assert_eq!((view.failing_since, view.error), (None, None));
}
