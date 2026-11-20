use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use serde_json::json;
use shoal_core::config::Config;
use shoal_protocol::hub::directory::{Report, SpaceSummary};

use super::*;
use crate::{
    stand_in::{self, StandIn},
    streams::Outgoing,
    test_support::linked,
};

/// A hub, on `config`, that records the machines `members`.
async fn hub(root: &tempfile::TempDir, config: &str, members: &[&str]) -> Arc<Hub> {
    let hub = stand_in::hub(root, Config::from_toml(config).unwrap()).await;
    std::fs::create_dir_all(&hub.paths.state).unwrap();
    for (index, member) in members.iter().enumerate() {
        let member = MemberId::from((*member).to_owned());
        let key = URL_SAFE_NO_PAD.encode([index as u8; 32]);
        (hub.store)
            .write(move |tx| {
                tx.execute(
                    "INSERT INTO hub_members(id, kind, name, public_key, joined_at)
                     VALUES (?1,'machine',?1,?2,0)",
                    params![member, key],
                )?;
                Ok(())
            })
            .await
            .unwrap();
    }
    hub
}

fn summary(id: &str) -> SpaceSummary {
    serde_json::from_value(json!({
        "id": id, "number": 1, "name": id, "kind": "chat", "status": "working",
    }))
    .unwrap()
}

/// Record `member`'s report that it has the spaces `ids`.
async fn report(hub: &Hub, member: &str, ids: &[&str]) {
    let spaces = ids.iter().map(|id| summary(id)).collect();
    let member = MemberId::from(member.to_owned());
    (hub.record_report(&StandIn, &member, Report::Whole(spaces), Vec::new()))
        .await
        .unwrap();
}

/// What the hub answers `member` sending `bytes` as its package of `space`,
/// once the hub ends the stream.
async fn send(hub: &Arc<Hub>, member: &str, space: &str, bytes: &[u8]) -> Result<()> {
    let link = linked();
    let (mut answer, mut package): (Incoming, Outgoing) = link.opener.streams().open_duplex();
    let sent = Sent {
        member: MemberId::from(member.to_owned()),
        space: SpaceId::from(space.to_owned()),
        built_at: UnixTime::now(),
    };
    let streams = (link.writer.streams(), answer.number());
    hub.keep_package(&StandIn, sent, streams).await?;
    package.write(bytes).await.unwrap();
    package.end(None).await;
    while answer.next().await?.is_some() {}
    Ok(())
}

fn kept(hub: &Hub, space: &str) -> Option<Vec<u8>> {
    std::fs::read(hub.package_path(&SpaceId::from(space.to_owned()))).ok()
}

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
