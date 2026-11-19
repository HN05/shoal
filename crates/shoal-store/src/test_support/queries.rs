//! A database with many owners, where a query of one owner's rows shows
//! whether it reads only that owner's or every owner's.
use anyhow::Result;
use rusqlite::{Connection, StatementStatus};

pub(crate) fn populated_database() -> Result<Connection> {
    let db = Connection::open_in_memory()?;
    db.execute_batch(crate::test_support::SCHEMA_V17)?;
    db.execute_batch(
        "PRAGMA foreign_keys=ON;
        CREATE TABLE repository_resource_leases (lease_id TEXT PRIMARY KEY, repository_id TEXT, path TEXT);
        INSERT INTO resource_pools VALUES ('global','pool','{}');",
    )?;
    add_owners(&db, 0, 10)?;
    Ok(db)
}

/// The column naming an execution's owner: its workspace before the
/// upgrade, its space after.
pub(crate) fn execution_space(db: &Connection) -> &'static str {
    match db.prepare("SELECT space_id FROM executions") {
        Ok(_) => "space_id",
        Err(_) => "workspace_id",
    }
}

/// Whether a space's shared fields have their own table, as they do after
/// the upgrade.
pub(crate) fn has_spaces(db: &Connection) -> bool {
    db.prepare("SELECT kind FROM spaces").is_ok()
}

fn add_owners(db: &Connection, start: i64, end: i64) -> Result<()> {
    for n in start..end {
        let owner = format!("owner-{n}");
        db.execute(
            "INSERT INTO repositories(id,path,source,last_used) VALUES (?1,?1,?1,0)",
            [&owner],
        )?;
        if has_spaces(db) {
            db.execute(
                "INSERT INTO spaces(id,kind,repository_id,name) VALUES (?1,'workspace',?1,?1)",
                [&owner],
            )?;
            db.execute(
                "INSERT INTO workspaces(id,path,branch,state) VALUES (?1,?1,'main','ready')",
                [&owner],
            )?;
        } else {
            db.execute(
                "INSERT INTO workspaces(id,repository_id,name,path,branch,state)
                VALUES (?1,?1,?1,?1,'main','ready')",
                [&owner],
            )?;
        }
        // Insert in reverse name order to exercise the explicit list ordering.
        for name in ["z", "a"] {
            let id = format!("{owner}-{name}");
            let port = 1000 + n * 2 + i64::from(name == "a");
            db.execute(
                "INSERT INTO ports VALUES (?1,?2,?3,?2,'retained')",
                rusqlite::params![owner, name, port],
            )?;
            db.execute("INSERT INTO resource_leases(id,workspace_id,scope,pool,name,resource,reason,created_at,mode)
                VALUES (?1,?2,'global','pool',?3,?3,'retained',123,'read')",
                [&id, &owner, name])?;
            db.execute(
                &format!(
                    "INSERT INTO executions(id,{},state) VALUES (?1,?2,'running')",
                    execution_space(db)
                ),
                [&id, &owner],
            )?;
        }
    }
    Ok(())
}

pub fn check_owner_query(sql: &str, table: &str, owner_column: &str) -> Result<()> {
    let db = lease_database()?;
    check_query_growth(&db, sql, table, owner_column, "owner-0")
}

/// The populated database with the columns lease queries read that the
/// v17 snapshot lacks.
pub fn lease_database() -> Result<Connection> {
    let db = populated_database()?;
    db.execute_batch(super::LEASE_ACQUIRERS)?;
    Ok(db)
}

pub(crate) fn check_query_growth(
    db: &Connection,
    sql: &str,
    table: &str,
    owner_column: &str,
    owner: &str,
) -> Result<()> {
    let mut baseline = None;
    for end in [10, 100, 1000] {
        if end > 10 {
            add_owners(db, end / 10, end)?;
        }
        let plan = db
            .prepare(&format!("EXPLAIN QUERY PLAN {sql}"))?
            .query_map([owner], |row| row.get::<_, String>(3))?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        assert!(
            plan.iter()
                .any(|step| step.contains(&format!("SEARCH {table}"))
                    && step.contains("INDEX")
                    && step.contains(&format!("{owner_column}=?"))),
            "{plan:?}"
        );
        let mut statement = db.prepare(sql)?;
        let rows = statement
            .query_map([owner], |row| row.get::<_, rusqlite::types::Value>(0))?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        assert!(!rows.is_empty());
        assert_eq!(statement.get_status(StatementStatus::FullscanStep), 0);
        let steps = statement.get_status(StatementStatus::VmStep);
        eprintln!(
            "{table}: {end} owners, {} rows, {steps} VM steps; {plan:?}",
            rows.len()
        );
        if let Some((expected_rows, initial_steps)) = &baseline {
            assert_eq!(&rows, expected_rows);
            // Allow planner/VM overhead changes, but not work linear in other owners.
            assert!(
                steps <= initial_steps * 2,
                "{table}: {initial_steps} -> {steps}"
            );
        } else {
            baseline = Some((rows, steps));
        }
    }
    Ok(())
}
