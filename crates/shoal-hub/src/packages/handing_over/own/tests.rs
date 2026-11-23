use crate::{
    packages::{
        Continuation,
        tests::{hub, kept, report},
    },
    stand_in::StandIn,
};

use super::*;

/// A paused hub's own package of `space`, as it keeps one to hand the
/// space over.
async fn keep_own(hub: &Hub, own: &MemberId, space: &SpaceId) {
    std::fs::create_dir_all(hub.package_directory()).unwrap();
    let incoming = tempfile::tempdir_in(hub.package_directory()).unwrap();
    let file = incoming.path().join("package.shoal");
    std::fs::write(&file, b"plan").unwrap();
    let sent = Sent {
        member: own.clone(),
        space: space.clone(),
        built_at: UnixTime::now(),
        arrived_at: UnixTime::now(),
        continuation: Continuation::default(),
    };
    let incoming_file = tempfile::TempPath::try_from_path(&file).unwrap();
    hub.place_package(sent, 4, incoming_file).await.unwrap();
}

#[tokio::test]
async fn only_the_machine_the_hub_s_own_handed_a_space_to_takes_it_over_until_it_unpauses() {
    let root = tempfile::tempdir().unwrap();
    let hub = hub(&root, "[machine.hub]\nenabled = true\n", &["desk"]).await;
    let own = hub.own_member().await.unwrap();
    report(&hub, own.as_str(), &["plan"]).await;
    let (plan, desk) = (
        SpaceId::from("plan".to_owned()),
        MemberId::from("desk".to_owned()),
    );
    keep_own(&hub, &own, &plan).await;
    // The hub's own machine is always connected, so its package is no
    // takeover's of a machine away.
    let takeover = hub.record_space_claim(&desk, &plan, (None, true)).await;
    assert!(takeover.is_err());
    hub.handovers.start(&plan, &desk);
    (hub.record_space_claim(&desk, &plan, (None, true)))
        .await
        .unwrap();
    // Unpaused, the hub retires its copy and forgets the takeover.
    hub.stay_own(&StandIn::default()).await;
    let taken = (hub.store).read(|tx| {
        let count = "SELECT COUNT(*) FROM hub_takeovers";
        Ok(tx.query_row(count, [], |row| row.get::<_, i64>(0))?)
    });
    assert_eq!(taken.await.unwrap(), 0);
    assert_eq!(kept(&hub, "plan"), None);
}
