use super::*;
use crate::packages::tests::{hub, kept, report, send};

#[tokio::test]
async fn the_hub_keeps_the_latest_whole_package_of_a_space_its_machine_has() {
    let root = tempfile::tempdir().unwrap();
    let hub = hub(&root, "", &["laptop"]).await;
    report(&hub, "laptop", &["plan"]).await;
    send(&hub, "laptop", "plan", b"first").await.unwrap();
    send(&hub, "laptop", "plan", b"second").await.unwrap();
    assert_eq!(kept(&hub, "plan").as_deref(), Some(&b"second"[..]));
}

#[tokio::test]
async fn a_package_of_a_space_another_machine_has_is_refused() {
    let root = tempfile::tempdir().unwrap();
    let hub = hub(&root, "", &["laptop", "desk"]).await;
    report(&hub, "desk", &["plan"]).await;
    report(&hub, "laptop", &["plan"]).await;
    let refused = send(&hub, "laptop", "plan", b"copy").await;
    assert!(refused.is_err());
    assert_eq!(kept(&hub, "plan"), None);
}

#[tokio::test]
async fn a_package_goes_once_its_space_ends_on_its_machine() {
    let root = tempfile::tempdir().unwrap();
    let hub = hub(&root, "", &["laptop"]).await;
    report(&hub, "laptop", &["plan", "fix"]).await;
    send(&hub, "laptop", "plan", b"plan").await.unwrap();
    send(&hub, "laptop", "fix", b"fix").await.unwrap();
    report(&hub, "laptop", &["fix"]).await;
    assert_eq!(kept(&hub, "plan"), None);
    assert_eq!(kept(&hub, "fix").as_deref(), Some(&b"fix"[..]));
}

#[tokio::test]
async fn revoking_a_machine_deletes_its_packages() {
    let root = tempfile::tempdir().unwrap();
    let hub = hub(&root, "", &["laptop"]).await;
    report(&hub, "laptop", &["plan"]).await;
    send(&hub, "laptop", "plan", b"plan").await.unwrap();
    hub.revoke_member("laptop", None).await.unwrap();
    assert_eq!(kept(&hub, "plan"), None);
}

#[tokio::test]
async fn a_hub_whose_disk_is_low_refuses_packages_and_alerts_once() {
    let root = tempfile::tempdir().unwrap();
    // No test machine's disk has a PiB free.
    let low = "[machine.overload.warning]\ndisk_free_gib = 1048576\n";
    let hub = hub(&root, low, &["laptop"]).await;
    report(&hub, "laptop", &["plan"]).await;
    for _ in 0..2 {
        assert!(send(&hub, "laptop", "plan", b"plan").await.is_err());
    }
    let alerts = hub.notifications.list(false, 50).await.unwrap();
    let refused = (alerts.iter())
        .filter(|alert| alert.kind == AlertKind::HubPackageRefused)
        .count();
    assert_eq!(refused, 1);
    assert_eq!(kept(&hub, "plan"), None);
}

#[tokio::test]
async fn a_package_goes_once_another_machine_takes_its_space_s_id_over() {
    let root = tempfile::tempdir().unwrap();
    let hub = hub(&root, "", &["laptop", "desk"]).await;
    report(&hub, "laptop", &["plan"]).await;
    send(&hub, "laptop", "plan", b"plan").await.unwrap();
    let (desk, plan) = (
        MemberId::from("desk".to_owned()),
        SpaceId::from("plan".to_owned()),
    );
    hub.record_space_claim(&desk, &plan, (None, false))
        .await
        .unwrap();
    // Its arrival there, as a move's, makes the space desk's.
    report(&hub, "desk", &["plan"]).await;
    assert_eq!(kept(&hub, "plan"), None);
}
