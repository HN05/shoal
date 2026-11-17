use serde_json::json;
use shoal_ids::member_ids::MemberId;

use super::{ALERT_EVERY, RefusalAlerts, claimed_version};

#[tokio::test(start_paused = true)]
async fn the_hub_alerts_to_each_refused_member_at_most_once_a_while() {
    let alerts = RefusalAlerts::default();
    let member = MemberId::generate();
    assert!(alerts.due(None));
    assert!(!alerts.due(None));
    assert!(alerts.due(Some(member.clone())));
    assert!(!alerts.due(Some(member.clone())));
    tokio::time::advance(ALERT_EVERY).await;
    assert!(alerts.due(None));
    assert!(alerts.due(Some(member)));
}

#[test]
fn a_claimed_version_stays_short_and_printable() {
    let version = |text: &str| claimed_version(&json!({ "version": text }));
    assert_eq!(version("0.18.1"), "0.18.1");
    assert_eq!(version("0.1\u{1b}[2J\n"), "0.12J");
    assert!(version(&"9".repeat(1000)).len() <= 32);
    assert_eq!(claimed_version(&json!({})), "of another release");
}
