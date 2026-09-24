use super::*;
use rusqlite::StatementStatus;
use std::{hint::black_box, time::Instant};

const INDEX_SCHEMA: &str = include_str!("../../../daemon/store/simulator_owner.sql");

fn owner(index: usize) -> String {
    format!("10000000-0000-0000-0000-{index:012}")
}

fn populate(db: &mut Connection, count: usize) -> Result<()> {
    let tx = db.transaction()?;
    for index in (0..count).rev() {
        let id = format!("00000000-0000-0000-0000-{index:012}");
        let current = owner(index / 2);
        // Two records per owner; unrelated claimed devices last belonged to 0.
        let mut sim = record(&id, Some(&current), Some(&owner(0)));
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
    records.retain(|s| s.workspace_id.as_deref().or(s.last_workspace_id.as_deref()) == Some(owner));
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
            serde_json::to_value(legacy_list(&db, &owner(0))?)?
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

fn median_us(rounds: usize, mut operation: impl FnMut() -> Result<usize>) -> Result<f64> {
    let mut samples = Vec::new();
    for _ in 0..9 {
        let start = Instant::now();
        for _ in 0..rounds {
            black_box(operation()?);
        }
        samples.push(start.elapsed().as_secs_f64() * 1e6 / rounds as f64);
    }
    samples.sort_by(f64::total_cmp);
    Ok(samples[4])
}

// Run with: cargo test --release --bin shoal benchmark_simulator_lookup -- --ignored --nocapture
// Warm temporary file DB, bundled SQLite, prepare per call, no Store scheduling
// or simctl. Timing is observational; only result equality is an assertion.
#[test]
#[ignore = "manual release-mode persistence microbenchmark"]
fn benchmark_simulator_lookup() -> Result<()> {
    println!(
        "rows,old_lookup_us,indexed_lookup_us,old_sweep_us,indexed_sweep_us,migration_us,old_write_us,indexed_write_us"
    );
    for size in [4, 16, 64, 256, 1_000, 10_000] {
        let root = tempfile::tempdir()?;
        let mut db = Connection::open(root.path().join("state.db"))?;
        db.execute_batch("CREATE TABLE simulators(id TEXT PRIMARY KEY, record TEXT NOT NULL)")?;
        populate(&mut db, size)?;
        let updated = record(
            "00000000-0000-0000-0000-000000000000",
            Some("new-owner"),
            Some(&owner(0)),
        );
        let old_write = measure_write(&mut db, &updated)?;
        let migration = median_us(1, || {
            let tx = db.transaction()?;
            list(&tx, None)?;
            tx.execute_batch(INDEX_SCHEMA)?;
            tx.rollback()?;
            Ok(size)
        })?;
        db.execute_batch(INDEX_SCHEMA)?;
        let indexed_write = measure_write(&mut db, &updated)?;
        let expected = serde_json::to_value(legacy_list(&db, &owner(0))?)?;
        assert_eq!(serde_json::to_value(list(&db, Some(&owner(0)))?)?, expected);
        let rounds = (10_000 / size).clamp(10, 500);
        let old = median_us(rounds, || Ok(legacy_list(&db, &owner(0))?.len()))?;
        let indexed = median_us(rounds, || Ok(list(&db, Some(&owner(0)))?.len()))?;
        // A sweep models one lookup per owner, not full workspace cleanup.
        let sweep = |indexed| -> Result<usize> {
            let mut total = 0;
            for index in 0..size / 2 {
                total += if indexed {
                    list(&db, Some(&owner(index)))?
                } else {
                    legacy_list(&db, &owner(index))?
                }
                .len();
            }
            assert_eq!(total, size);
            Ok(total)
        };
        let (old_sweep, indexed_sweep) = if size <= 256 {
            (
                median_us(1, || sweep(false))?,
                median_us(1, || sweep(true))?,
            )
        } else {
            (f64::NAN, f64::NAN)
        };
        println!(
            "{size},{old:.2},{indexed:.2},{old_sweep:.2},{indexed_sweep:.2},{migration:.2},{old_write:.2},{indexed_write:.2}"
        );
    }
    Ok(())
}

fn measure_write(db: &mut Connection, sim: &Value) -> Result<f64> {
    median_us(20, || {
        let tx = db.transaction()?;
        save(&tx, sim)?;
        tx.rollback()?;
        Ok(1)
    })
}
