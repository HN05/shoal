use super::*;
use crate::client::space_ids::SpaceId;
use rusqlite::StatementStatus;

const INDEX_SCHEMA: &str = include_str!("../../../store/simulator_owner.sql");

fn owner(index: usize) -> SpaceId {
    SpaceId::from(format!("10000000-0000-0000-0000-{index:012}"))
}

fn populate(db: &mut Connection, count: usize) -> Result<()> {
    let tx = db.transaction()?;
    for index in (0..count).rev() {
        let id = format!("00000000-0000-0000-0000-{index:012}");
        let current = owner(index / 2);
        // Two records per owner; unrelated claimed devices last belonged to 0.
        let mut sim = record(&id, Some(current.as_str()), Some(owner(0).as_str()));
        if index % 2 == 1 {
            sim["workspace_id"] = Value::Null;
            sim["last_workspace_id"] = json!(current);
        }
        sim["udid"] = json!(id);
        sim["device"] = json!("com.apple.CoreSimulator.SimDeviceType.iPhone-17");
        sim["runtime"] = json!("com.apple.CoreSimulator.SimRuntime.iOS-27-0");
        save(&tx, &sim)?;
    }
    tx.commit()?;
    Ok(())
}

// Preserve the former collect/decode/retain path for a comparable baseline.
fn legacy_list(db: &Connection, owner: &str) -> Result<Vec<Simulator>> {
    let records = db
        .prepare("SELECT record FROM simulators ORDER BY id")?
        .query_map([], |r| r.get::<_, String>(0))?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    let mut records = records
        .into_iter()
        .map(|r| serde_json::from_str::<Simulator>(&r))
        .collect::<serde_json::Result<Vec<_>>>()?;
    records.retain(|s| {
        s.workspace_id
            .as_ref()
            .map(SpaceId::as_str)
            .or(s.last_workspace_id.as_ref().map(SpaceId::as_str))
            == Some(owner)
    });
    Ok(records)
}

#[test]
fn indexed_plan_and_decode_counts_stay_bounded_as_inventory_grows() -> Result<()> {
    for size in [4, 16, 64, 256, 1_000, 10_000] {
        let mut db = Connection::open_in_memory()?;
        db.execute_batch("CREATE TABLE simulators(id TEXT PRIMARY KEY, record TEXT NOT NULL)")?;
        db.execute_batch(INDEX_SCHEMA)?;
        populate(&mut db, size)?;
        let plan = db
            .prepare(&format!("EXPLAIN QUERY PLAN {OWNED}"))?
            .query_map([owner(0)], |row| row.get::<_, String>(3))?
            .collect::<rusqlite::Result<Vec<_>>>()?
            .join("\n");
        assert!(
            plan.contains("SEARCH simulators USING INDEX simulators_effective_owner"),
            "{plan}"
        );
        assert!(!plan.contains("TEMP B-TREE"), "{plan}");
        let mut statement = db.prepare(OWNED)?;
        assert_eq!(statement.query_map([owner(0)], |_| Ok(()))?.count(), 2);
        assert_eq!(statement.get_status(StatementStatus::FullscanStep), 0);
        assert_eq!(statement.get_status(StatementStatus::Sort), 0);
        DECODED_RECORDS.set(0);
        let selected = list(&db, Some(&owner(0)))?;
        assert_eq!(DECODED_RECORDS.get(), 2, "inventory {size}");
        assert_eq!(
            serde_json::to_value(selected)?,
            serde_json::to_value(legacy_list(&db, owner(0).as_str())?)?
        );
        DECODED_RECORDS.set(0);
        for index in size..size + 100 {
            assert!(list(&db, Some(&owner(index)))?.is_empty());
        }
        assert_eq!(DECODED_RECORDS.get(), 0);
        assert_eq!(list(&db, None)?.len(), size);
        assert_eq!(DECODED_RECORDS.get(), size);
    }
    Ok(())
}
