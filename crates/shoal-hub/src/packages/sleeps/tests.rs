use std::time::Duration;

use serde_json::json;
use shoal_protocol::hub::{
    Member, MemberKind,
    availability::{AvailabilityReason, Classified},
    directory::Listed,
};

use super::*;
use crate::packages::tests::{hub, report, send};

fn id(id: &str) -> SpaceId {
    SpaceId::from(id.to_owned())
}

/// Each space the hub records `member`'s packages lagging for, with
/// since when, and when it recorded `member` asleep.
async fn recorded(hub: &Hub, member: &str) -> (Option<UnixTime>, Vec<(String, Option<UnixTime>)>) {
    let member = MemberId::from(member.to_owned());
    (hub.store)
        .read(move |tx| {
            let asleep = tx.query_row(
                "SELECT asleep_at FROM hub_members WHERE id=?1",
                [&member],
                |row| row.get(0),
            )?;
            let mut lags = tx.prepare(
                "SELECT space_id, since FROM hub_sleep_lags WHERE member_id=?1 ORDER BY space_id",
            )?;
            let lags = lags.query_map([&member], |row| Ok((row.get(0)?, row.get(1)?)))?;
            Ok((asleep, lags.collect::<rusqlite::Result<_>>()?))
        })
        .await
        .unwrap()
}

#[tokio::test]
async fn a_package_built_after_the_notice_catches_its_space_up_until_the_link_opens() {
    let root = tempfile::tempdir().unwrap();
    let hub = hub(&root, "", &["laptop"]).await;
    report(&hub, "laptop", &["plan", "fix"]).await;
    let laptop = MemberId::from("laptop".to_owned());
    let behind = UnixTime::from_seconds(100);
    let lagging = vec![
        Lagging {
            space: id("fix"),
            since: Some(behind),
        },
        Lagging {
            space: id("plan"),
            since: None,
        },
    ];
    hub.hear_sleep(&laptop, lagging).await.unwrap();
    let (asleep, lags) = recorded(&hub, "laptop").await;
    assert!(asleep.is_some());
    assert_eq!(
        lags,
        [("fix".to_owned(), Some(behind)), ("plan".to_owned(), None)]
    );
    send(&hub, "laptop", "plan", b"package").await.unwrap();
    let (_, lags) = recorded(&hub, "laptop").await;
    assert_eq!(lags, [("fix".to_owned(), Some(behind))]);
    hub.hear_awake(&laptop).await;
    assert_eq!(recorded(&hub, "laptop").await, (None, Vec::new()));
}

#[test]
fn only_a_report_on_a_nearly_empty_battery_makes_a_drop_a_sleep() {
    let sleeps = Sleeps::default();
    let laptop = MemberId::from("laptop".to_owned());
    let reported = |on_battery, percent| {
        let power = Power {
            on_battery,
            percent,
        };
        sleeps.powered(&laptop, Some(power));
        sleeps.ran_out_at(&laptop).is_some()
    };
    assert!(!reported(true, 5));
    assert!(!reported(false, 1));
    assert!(reported(true, 4));
    sleeps.powered(&laptop, None);
    assert!(sleeps.ran_out_at(&laptop).is_none());
}

fn machine(name: &str, availability: Availability) -> Member {
    Member {
        id: MemberId::from(name.to_owned()),
        kind: MemberKind::Machine,
        name: name.into(),
        version: None,
        joined_at: UnixTime::from_seconds(0),
        last_seen: None,
        connected: false,
        hub: false,
        availability: Some(Classified {
            availability,
            reason: AvailabilityReason::Set,
        }),
        agents: Vec::new(),
        capacity: None,
    }
}

fn listed(machine: &str, id: &str, unsent_since: Option<UnixTime>) -> Listed {
    serde_json::from_value(json!({
        "machine": machine, "id": id, "number": 1, "name": id, "kind": "chat",
        "status": "inactive", "reported_at": 0, "unsent_since": unsent_since,
    }))
    .unwrap()
}

#[test]
fn a_battery_drop_lags_as_the_last_report_said_and_unknown_where_no_package_is_kept_current() {
    let behind = UnixTime::now() - Duration::from_secs(60);
    let directory = Directory {
        machines: vec![
            machine("laptop", Availability::Intermittent),
            machine("desk", Availability::AlwaysOn),
        ],
        spaces: vec![
            listed("laptop", "fix", Some(behind)),
            listed("laptop", "plan", None),
            listed("desk", "build", None),
        ],
        ..Directory::default()
    };
    let laptop = MemberId::from("laptop".to_owned());
    let lagging = Lagging {
        space: id("fix"),
        since: Some(behind),
    };
    assert_eq!(reported_lags(&directory, &laptop), [lagging]);
    let desk = MemberId::from("desk".to_owned());
    let unknown = Lagging {
        space: id("build"),
        since: None,
    };
    assert_eq!(reported_lags(&directory, &desk), [unknown]);
}
