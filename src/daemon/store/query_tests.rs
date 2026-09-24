use super::*;
use crate::daemon::resources;
use rusqlite::StatementStatus;

fn populated_database() -> Result<Connection> {
    let db = Connection::open_in_memory()?;
    db.execute_batch(include_str!("../../../tests/fixtures/schema_v17.sql"))?;
    db.execute_batch(
        "PRAGMA foreign_keys=ON;
        INSERT INTO resource_pools VALUES ('global','pool','{}');",
    )?;
    add_owners(&db, 0, 10)?;
    Ok(db)
}

fn add_owners(db: &Connection, start: i64, end: i64) -> Result<()> {
    for n in start..end {
        let owner = format!("owner-{n}");
        db.execute(
            "INSERT INTO repositories(id,path,source,last_used) VALUES (?1,?1,?1,0)",
            [&owner],
        )?;
        db.execute(
            "INSERT INTO workspaces(id,repository_id,name,path,branch,state)
            VALUES (?1,?1,?1,?1,'main','ready')",
            [&owner],
        )?;
        // Insert in reverse name order to exercise the explicit list ordering.
        for name in ["z", "a"] {
            let id = format!("{owner}-{name}");
            let port = 1000 + n * 2 + i64::from(name == "a");
            db.execute(
                "INSERT INTO ports VALUES (?1,?2,?3,?2,'retained')",
                rusqlite::params![owner, name, port],
            )?;
            db.execute("INSERT INTO resource_leases VALUES (?1,?2,'global','pool',?3,?3,'retained',123,'read')",
                [&id, &owner, name])?;
            db.execute(
                "INSERT INTO executions(id,workspace_id,state) VALUES (?1,?2,'running')",
                [&id, &owner],
            )?;
        }
    }
    Ok(())
}

pub(in crate::daemon) fn check_owner_query(
    sql: &str,
    table: &str,
    owner_column: &str,
) -> Result<()> {
    let db = populated_database()?;
    check_query_growth(&db, sql, table, owner_column, "owner-0")
}

fn check_query_growth(
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

#[test]
fn owner_port_reads_search_existing_index() -> Result<()> {
    check_owner_query(&ports_query(true), "ports", "workspace_id")
}

#[test]
fn resource_lists_preserve_global_order_and_owner_selection() -> Result<()> {
    let db = populated_database()?;
    let all_ports = ports(&db, None)?;
    let all_leases = resources::leases(&db, None)?;
    assert_eq!(all_ports.len(), 20);
    assert_eq!(all_leases.len(), 20);
    assert!(all_ports.windows(2).all(
        |pair| (&pair[0].workspace_id, &pair[0].name) < (&pair[1].workspace_id, &pair[1].name)
    ));
    let lease_key = |lease: &resources::ResourceLease| {
        (
            lease.scope.to_string(),
            lease.pool.clone(),
            lease.resource.clone(),
            lease.name.clone(),
            lease.id.clone(),
        )
    };
    assert!(
        all_leases
            .windows(2)
            .all(|pair| lease_key(&pair[0]) < lease_key(&pair[1]))
    );
    for owner in ["owner-0", "owner-9", "missing", ""] {
        let expected_ports: Vec<_> = all_ports
            .iter()
            .filter(|p| p.workspace_id == owner)
            .collect();
        let expected_leases: Vec<_> = all_leases
            .iter()
            .filter(|l| l.workspace_id == owner)
            .collect();
        assert_eq!(
            serde_json::to_value(ports(&db, Some(owner))?)?,
            serde_json::to_value(expected_ports)?
        );
        assert_eq!(
            serde_json::to_value(resources::leases(&db, Some(owner))?)?,
            serde_json::to_value(expected_leases)?
        );
    }
    Ok(())
}

#[test]
fn resource_lists_propagate_decoding_and_prepare_errors() -> Result<()> {
    let db = populated_database()?;
    db.execute_batch(
        "PRAGMA ignore_check_constraints=ON;
        UPDATE ports SET port=-port WHERE workspace_id='owner-0';
        UPDATE resource_leases SET mode='invalid' WHERE workspace_id='owner-0';",
    )?;
    for owner in [None, Some("owner-0")] {
        assert!(ports(&db, owner).is_err());
        assert!(resources::leases(&db, owner).is_err());
    }
    assert_eq!(ports(&db, Some("owner-1"))?.len(), 2);
    assert_eq!(resources::leases(&db, Some("owner-1"))?.len(), 2);
    db.execute_batch("DROP TABLE ports; DROP TABLE resource_leases;")?;
    for owner in [None, Some("owner-0")] {
        assert!(ports(&db, owner).is_err());
        assert!(resources::leases(&db, owner).is_err());
    }
    Ok(())
}

#[test]
fn execution_owner_reads_search_migrated_index() -> Result<()> {
    let mut db = populated_database()?;
    migrate(&mut db)?;
    check_query_growth(
        &db,
        &executions_query(),
        "executions",
        "workspace_id",
        "owner-0",
    )
}

#[test]
fn repository_workspace_reads_search_migrated_index() -> Result<()> {
    let mut db = populated_database()?;
    migrate(&mut db)?;
    // A missing repository exercises the entire lookup even with EXISTS short-circuiting.
    check_query_growth(
        &db,
        &format!("SELECT EXISTS({REPOSITORY_WORKSPACES_QUERY})"),
        "workspaces",
        "repository_id",
        "missing",
    )?;
    assert!(!repository_has_workspaces(&db, "missing")?);
    assert!(repository_has_workspaces(&db, "owner-0")?);
    Ok(())
}

fn ownership_snapshot(db: &mut Connection) -> Result<Vec<Vec<Vec<rusqlite::types::Value>>>> {
    [
        "repositories",
        "workspaces",
        "ports",
        "resource_pools",
        "resource_leases",
        "executions",
    ]
    .into_iter()
    .map(|table| {
        let mut statement = db.prepare(&format!("SELECT * FROM {table} ORDER BY rowid"))?;
        let columns = statement.column_count();
        Ok(statement
            .query_map([], |row| (0..columns).map(|i| row.get(i)).collect())?
            .collect::<rusqlite::Result<_>>()?)
    })
    .collect()
}

#[tokio::test]
async fn v17_upgrade_and_reopen_preserve_ownership_leases_and_executions() -> Result<()> {
    let root = tempfile::tempdir()?;
    let path = root.path().join("state.db");
    let mut old = populated_database()?;
    old.execute_batch(
        r#"UPDATE workspaces SET state='preparing', git_dir='/git/worktrees/owner',
            git_dir_id='1:2', base_commit='abc', base_ref='refs/heads/main';
        UPDATE executions SET wrapper='{"pid":123,"birth":"wrapper"}',
            child='{"pid":456,"birth":"child"}', group_id=456;"#,
    )?;
    let before = ownership_snapshot(&mut old)?;
    old.execute("VACUUM INTO ?1", [path.to_str().unwrap()])?;
    drop(old);
    for _ in 0..2 {
        let reopened = Store::open(path.clone()).await?;
        assert_eq!(reopened.run(ownership_snapshot).await?, before);
        reopened
            .run(|db| {
                assert_eq!(
                    db.query_row("PRAGMA user_version", [], |row| row.get::<_, i64>(0))?,
                    SCHEMA_VERSION
                );
                for (index, column) in [
                    ("executions_workspace", "workspace_id"),
                    ("workspaces_repository", "repository_id"),
                ] {
                    let columns = db
                        .prepare(&format!("PRAGMA index_info({index})"))?
                        .query_map([], |row| row.get::<_, String>(2))?
                        .collect::<rusqlite::Result<Vec<_>>>()?;
                    assert_eq!(columns, [column]);
                }
                assert_eq!(executions(db, "owner-0")?[0].state, ExecutionState::Running);
                assert_eq!(ports(db, Some("owner-0"))?.len(), 2);
                assert_eq!(resources::leases(db, Some("owner-0"))?.len(), 2);
                Ok(())
            })
            .await?;
    }
    Ok(())
}
