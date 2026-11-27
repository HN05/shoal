use super::*;
use crate::store::Store;
use serde_json::{Value, json};
use shoal_ids::space_ids::SpaceId;
use shoal_protocol::sim::SimulatorState;
use shoal_store::test_support::open_database;

fn record(id: &str, current: Option<&str>, last: Option<&str>) -> Simulator {
    Simulator {
        id: id.into(),
        udid: None,
        device: "phone".into(),
        runtime: "ios".into(),
        workspace_id: current.map(SpaceId::from),
        last_workspace_id: last.map(SpaceId::from),
        lease_name: Some("default".into()),
        reason: None,
        state: SimulatorState::Booting,
        last_used: UnixTime::from_seconds(42),
        error: None,
        installed_apps: None,
        acquirer: Acquirer::User,
    }
}

fn ids(db: &Connection, owner: Option<&SpaceId>) -> Result<Vec<String>> {
    Ok(list(db, owner)?.into_iter().map(|sim| sim.id).collect())
}

#[tokio::test]
async fn transitions_and_reopens_keep_ownership() -> Result<()> {
    let root = tempfile::tempdir()?;
    let path = root.path().join("state.db");
    let store = Store::open(path.clone()).await?;
    let db = open_database(&path)?;
    let mut sim = record("b", Some("A"), None);
    save(&db, &sim)?;
    save(&db, &record("a", None, Some("A")))?;
    save(&db, &record("c", None, None))?;
    assert_eq!(ids(&db, Some(&SpaceId::from("A")))?, ["a", "b"]);
    assert_eq!(ids(&db, None)?, ["a", "b", "c"]);
    for state in [
        SimulatorState::Creating,
        SimulatorState::Booting,
        SimulatorState::Leased,
        SimulatorState::Failed,
    ] {
        sim.state = state;
        save(&db, &sim)?;
        assert_eq!(ids(&db, Some(&SpaceId::from("A")))?, ["a", "b"]);
    }
    sim.workspace_id = None;
    sim.last_workspace_id = Some("A".into());
    sim.state = SimulatorState::Idle;
    save(&db, &sim)?;
    assert_eq!(ids(&db, Some(&SpaceId::from("A")))?, ["a", "b"]);

    // The persisted claim changes owner before boot updates the previous owner.
    sim.workspace_id = Some("B".into());
    sim.state = SimulatorState::Booting;
    save(&db, &sim)?;
    assert_eq!(ids(&db, Some(&SpaceId::from("A")))?, ["a"]);
    assert_eq!(ids(&db, Some(&SpaceId::from("B")))?, ["b"]);
    drop(store);
    drop(Store::open(path).await?);
    assert_eq!(ids(&db, Some(&SpaceId::from("B")))?, ["b"]);
    sim.last_workspace_id = Some("B".into());
    sim.state = SimulatorState::Failed;
    save(&db, &sim)?;
    assert_eq!(ids(&db, Some(&SpaceId::from("A")))?, ["a"]);
    assert_eq!(ids(&db, Some(&SpaceId::from("B")))?, ["b"]);
    sim.workspace_id = None;
    sim.state = SimulatorState::Idle;
    save(&db, &sim)?;
    assert_eq!(ids(&db, Some(&SpaceId::from("B")))?, ["b"]);
    delete(&db, "b")?;
    assert!(ids(&db, Some(&SpaceId::from("B")))?.is_empty());
    Ok(())
}

#[test]
fn a_simulator_round_trips_through_its_columns_and_record() -> Result<()> {
    let db = shoal_store::test_support::migrated()?;
    let mut sim = record("sim", Some("A"), Some("B"));
    (sim.udid, sim.reason, sim.error) = (Some("U".into()), Some("why".into()), Some("e".into()));
    (sim.state, sim.installed_apps) = (SimulatorState::Leased, Some(3));
    save(&db, &sim)?;

    let [saved] = &list(&db, None)?[..] else {
        anyhow::bail!("expected one simulator");
    };
    assert_eq!(serde_json::to_value(saved)?, serde_json::to_value(&sim)?);
    let (state, current, last, record): (String, String, String, String) = db.query_row(
        "SELECT state,workspace_id,last_workspace_id,record FROM simulators",
        [],
        |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
    )?;
    assert_eq!((&*state, &*current, &*last), ("leased", "A", "B"));
    let record: Value = serde_json::from_str(&record)?;
    for moved in ["id", "state", "workspace_id", "last_workspace_id"] {
        assert_eq!(record.get(moved), None, "{record}");
    }
    Ok(())
}

#[test]
fn a_row_that_does_not_decode_names_its_simulator() -> Result<()> {
    let db = shoal_store::test_support::migrated()?;
    save(&db, &record("owned", Some("A"), None))?;
    save(&db, &record("broken", Some("B"), None))?;
    db.execute(
        "UPDATE simulators SET state='unknown-state' WHERE id='broken'",
        [],
    )?;
    assert_eq!(ids(&db, Some(&SpaceId::from("A")))?, ["owned"]);
    for owner in [None, Some(SpaceId::from("B"))] {
        let diagnostic = format!("{:#}", list(&db, owner.as_ref()).unwrap_err());
        assert!(
            diagnostic.contains("decode simulator broken"),
            "{diagnostic}"
        );
        assert!(diagnostic.contains("unknown-state"), "{diagnostic}");
    }

    // A record that still holds a field its columns hold is refused.
    let stale = json!({"udid": null, "device": "phone", "runtime": "ios", "lease_name": null,
        "reason": null, "last_used": 42, "error": null, "installed_apps": null,
        "acquirer": {"kind": "user"}, "state": "idle"});
    db.execute(
        "UPDATE simulators SET state='idle', record=?1 WHERE id='broken'",
        [stale.to_string()],
    )?;
    let diagnostic = format!("{:#}", list(&db, None).unwrap_err());
    assert!(
        diagnostic.contains("decode simulator broken"),
        "{diagnostic}"
    );
    assert!(diagnostic.contains("unknown field `state`"), "{diagnostic}");
    Ok(())
}

mod performance;
