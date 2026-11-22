use super::*;
use crate::{
    packages::tests::{hub, report, send},
    test_support::linked,
};

/// Each space taken over, with the machine it was taken from.
async fn takeovers(hub: &Hub) -> Vec<(String, String)> {
    let taken = hub.store.read(|tx| {
        let mut statement = tx.prepare("SELECT space_id, member_id FROM hub_takeovers")?;
        let rows = statement.query_map([], |row| Ok((row.get(0)?, row.get(1)?)))?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    });
    taken.await.unwrap()
}

#[tokio::test]
async fn a_takeover_s_claim_needs_another_machine_s_package_while_it_is_offline() {
    let root = tempfile::tempdir().unwrap();
    let hub = hub(&root, "", &["laptop", "desk"]).await;
    report(&hub, "laptop", &["plan", "fix"]).await;
    send(&hub, "laptop", "plan", b"plan").await.unwrap();
    let (desk, laptop) = (
        MemberId::from("desk".to_owned()),
        MemberId::from("laptop".to_owned()),
    );
    let claim = |claimer: &MemberId, space: &str, takeover: bool| {
        let (hub, claimer, space) = (hub.clone(), claimer.clone(), space.to_owned());
        async move {
            let space = SpaceId::from(space);
            (hub.record_space_claim(&claimer, &space, (None, takeover))
                .await)
                .is_ok()
        }
    };
    // Without a package, or of the machine's own space, a takeover is
    // refused; a plain claim takes nothing over.
    assert!(!claim(&desk, "fix", true).await);
    assert!(!claim(&laptop, "plan", true).await);
    assert!(claim(&desk, "plan", false).await);
    assert!(takeovers(&hub).await.is_empty());
    assert!(claim(&desk, "plan", true).await);
    assert_eq!(takeovers(&hub).await, [("plan".into(), "laptop".into())]);
}

#[tokio::test]
async fn a_takeover_is_refused_once_the_space_s_machine_is_connected_again() {
    let root = tempfile::tempdir().unwrap();
    let hub = hub(&root, "", &["laptop", "desk"]).await;
    report(&hub, "laptop", &["plan"]).await;
    send(&hub, "laptop", "plan", b"plan").await.unwrap();
    let laptop = MemberId::from("laptop".to_owned());
    let connected = HashSet::from([laptop]);
    let taken = hub.store.write(move |tx| {
        let (plan, desk) = (
            SpaceId::from("plan".to_owned()),
            MemberId::from("desk".to_owned()),
        );
        record_takeover(tx, &plan, &desk, (&connected, false))
    });
    assert!(taken.await.is_err());
    assert!(takeovers(&hub).await.is_empty());
}

#[tokio::test]
async fn a_space_handed_over_is_taken_over_while_its_machine_is_still_connected() {
    let root = tempfile::tempdir().unwrap();
    let hub = hub(&root, "", &["laptop", "desk"]).await;
    report(&hub, "laptop", &["plan"]).await;
    send(&hub, "laptop", "plan", b"plan").await.unwrap();
    let laptop = MemberId::from("laptop".to_owned());
    let connected = HashSet::from([laptop]);
    let taken = hub.store.write(move |tx| {
        let (plan, desk) = (
            SpaceId::from("plan".to_owned()),
            MemberId::from("desk".to_owned()),
        );
        record_takeover(tx, &plan, &desk, (&connected, true))
    });
    taken.await.unwrap();
    assert_eq!(takeovers(&hub).await, [("plan".into(), "laptop".into())]);
}

#[tokio::test]
async fn the_hub_writes_an_offline_machine_s_package_to_the_machine_that_takes_it_over() {
    let root = tempfile::tempdir().unwrap();
    let hub = hub(&root, "", &["laptop", "desk"]).await;
    report(&hub, "laptop", &["plan"]).await;
    send(&hub, "laptop", "plan", b"the package").await.unwrap();
    let link = linked();
    let mut incoming = link.opener.streams().open();
    let space = SpaceId::from("plan".to_owned());
    let streams = (link.writer.streams(), incoming.number());
    let desk = MemberId::from("desk".to_owned());
    hub.serve_fetch(&desk, &space, streams).await.unwrap();
    let mut fetched = Vec::new();
    while let Some(bytes) = incoming.next().await.unwrap() {
        fetched.extend(bytes);
    }
    assert_eq!(fetched, b"the package");
    let none = SpaceId::from("fix".to_owned());
    let (_, streams) = (link.opener.streams().open(), link.writer.streams());
    assert!(hub.serve_fetch(&desk, &none, (streams, 99)).await.is_err());
}
