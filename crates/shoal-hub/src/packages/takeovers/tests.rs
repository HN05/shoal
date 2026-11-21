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
async fn another_machine_s_claim_takes_over_a_space_from_its_offline_machine_s_package() {
    let root = tempfile::tempdir().unwrap();
    let hub = hub(&root, "", &["laptop", "desk"]).await;
    report(&hub, "laptop", &["plan", "fix"]).await;
    send(&hub, "laptop", "plan", b"plan").await.unwrap();
    let (desk, laptop) = (
        MemberId::from("desk".to_owned()),
        MemberId::from("laptop".to_owned()),
    );
    for (claimer, space) in [(&desk, "plan"), (&desk, "fix"), (&laptop, "plan")] {
        let space = SpaceId::from(space.to_owned());
        hub.record_space_claim(claimer, &space, None).await.unwrap();
    }
    // A space without a package, and the machine's own claim, take nothing
    // over.
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
    hub.serve_fetch(&space, streams).await.unwrap();
    let mut fetched = Vec::new();
    while let Some(bytes) = incoming.next().await.unwrap() {
        fetched.extend(bytes);
    }
    assert_eq!(fetched, b"the package");
    let none = SpaceId::from("fix".to_owned());
    let (_, streams) = (link.opener.streams().open(), link.writer.streams());
    assert!(hub.serve_fetch(&none, (streams, 99)).await.is_err());
}
