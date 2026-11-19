use serde_json::json;
use shoal_ids::member_ids::MemberId;
use shoal_protocol::{hub::link::HubVersion, protocol::VERSION};

use super::{ALERT_EVERY, OWN_VERSION, RefusalAlerts, claimed_version, same_release};
use crate::{
    identity::{nonce, verify},
    stand_in::{self, StandIn},
};

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

#[test]
fn the_hub_takes_only_its_own_release() {
    let opening = |version: &str| json!({ "version": version });
    assert!(same_release(VERSION, &opening(OWN_VERSION)));
    assert!(!same_release(VERSION, &opening("0.0.1")));
    assert!(!same_release(VERSION + 1, &opening(OWN_VERSION)));
}

#[tokio::test]
async fn the_hub_signs_its_version_over_the_refused_members_nonce() {
    let root = tempfile::tempdir().unwrap();
    let hub = stand_in::hub(&root, shoal_core::config::Config::from_toml("").unwrap()).await;
    let (theirs, other) = (nonce(), nonce());
    let opening = json!({ "version": "0.0.1", "nonce": theirs });
    let refusal = hub.refuse_version(&StandIn, &opening, VERSION).await;
    let signed = refusal.version.unwrap();
    assert_eq!(signed.version, OWN_VERSION);
    assert_eq!(signed.key, hub.hub_identity().await.unwrap().public);
    let checks = |nonce| {
        verify(
            &signed.key,
            &HubVersion::signed(OWN_VERSION, nonce),
            &signed.signature,
        )
    };
    assert!(checks(&theirs));
    assert!(!checks(&other));
    assert!(
        refusal
            .message
            .contains(&format!("--version {OWN_VERSION}"))
    );
}
