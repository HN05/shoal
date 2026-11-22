use super::*;
use crate::{
    packages::tests::{hub, kept, report, send},
    stand_in::StandIn,
};

#[tokio::test]
async fn a_retired_takeover_goes_with_the_machine_s_package_and_the_others_wait() {
    let root = tempfile::tempdir().unwrap();
    let hub = hub(&root, "", &["laptop", "desk"]).await;
    report(&hub, "laptop", &["plan", "fix"]).await;
    send(&hub, "laptop", "plan", b"plan").await.unwrap();
    send(&hub, "laptop", "fix", b"fix").await.unwrap();
    let desk = MemberId::from("desk".to_owned());
    for space in ["plan", "fix"] {
        let space = SpaceId::from(space.to_owned());
        hub.record_space_claim(&desk, &space, (None, true))
            .await
            .unwrap();
    }
    let laptop = MemberId::from("laptop".to_owned());
    let retired = vec![SpaceId::from("plan".to_owned())];
    (hub.store)
        .write(move |tx| forget_retired(tx, &laptop, &retired))
        .await
        .unwrap();
    hub.delete_ended_packages().await;
    let laptop = MemberId::from("laptop".to_owned());
    let waiting = hub.store.read(move |tx| taken_from(tx, &laptop)).await;
    assert_eq!(waiting.unwrap(), [SpaceId::from("fix".to_owned())]);
    assert_eq!(kept(&hub, "plan"), None);
    assert_eq!(kept(&hub, "fix").as_deref(), Some(&b"fix"[..]));
}

#[tokio::test]
async fn a_space_the_returning_machine_no_longer_has_counts_as_retired() {
    let root = tempfile::tempdir().unwrap();
    let hub = hub(&root, "", &[]).await;
    let spaces = vec![SpaceId::from("plan".to_owned())];
    assert_eq!(
        hub.retire(&StandIn::default(), spaces.clone()).await,
        spaces
    );
}
