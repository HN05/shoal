use super::*;
use crate::test_support::queries::{
    check_owner_query, check_query_growth, execution_space, has_spaces, lease_database,
    populated_database,
};
use shoal_ids::space_ids::SpaceId;

#[test]
fn owner_port_reads_search_existing_index() -> Result<()> {
    check_owner_query(&ports_query(true), "ports", "workspace_id")
}

#[test]
fn port_lists_preserve_global_order_and_owner_selection() -> Result<()> {
    let db = lease_database()?;
    let all_ports = ports(&db, None)?;
    assert_eq!(all_ports.len(), 20);
    assert!(all_ports.windows(2).all(
        |pair| (&pair[0].workspace_id, &pair[0].name) < (&pair[1].workspace_id, &pair[1].name)
    ));
    for owner in ["owner-0", "owner-9", "missing", ""] {
        let expected_ports: Vec<_> = all_ports
            .iter()
            .filter(|p| p.workspace_id == owner)
            .collect();
        assert_eq!(
            serde_json::to_value(ports(&db, Some(&SpaceId::from(owner)))?)?,
            serde_json::to_value(expected_ports)?
        );
    }
    Ok(())
}

#[test]
fn port_lists_propagate_decoding_and_prepare_errors() -> Result<()> {
    let db = lease_database()?;
    db.execute_batch(
        "PRAGMA ignore_check_constraints=ON;
        UPDATE ports SET port=-port WHERE workspace_id='owner-0';",
    )?;
    for owner in [None, Some(SpaceId::from("owner-0"))] {
        assert!(ports(&db, owner.as_ref()).is_err());
    }
    assert_eq!(ports(&db, Some(&SpaceId::from("owner-1")))?.len(), 2);
    db.execute_batch("DROP TABLE ports;")?;
    for owner in [None, Some(SpaceId::from("owner-0"))] {
        assert!(ports(&db, owner.as_ref()).is_err());
    }
    Ok(())
}

#[test]
fn execution_owner_reads_search_migrated_index() -> Result<()> {
    let mut db = populated_database()?;
    super::tests::migrate_through_v0_17_0(&mut db)?;
    check_query_growth(
        &db,
        &executions_query(),
        "executions",
        "space_id",
        "owner-0",
    )
}

#[test]
fn repository_space_reads_search_migrated_index() -> Result<()> {
    let mut db = populated_database()?;
    super::tests::migrate_through_v0_17_0(&mut db)?;
    // A missing repository exercises the entire lookup even with EXISTS short-circuiting.
    check_query_growth(
        &db,
        &format!("SELECT EXISTS({REPOSITORY_SPACES_QUERY})"),
        "spaces",
        "repository_id",
        "missing",
    )?;
    assert!(!repository_has_spaces(&db, &"missing".into())?);
    assert!(repository_has_spaces(&db, &"owner-0".into())?);
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
        // Columns added after version 17 are absent before the upgrade.
        let execution_columns = format!("id,{},state,wrapper,child,group_id", execution_space(db));
        let columns = match table {
            "repositories" => "id,path,source,last_used,name,workspaces_dir",
            "workspaces" => {
                "id,repository_id,name,path,branch,state,error,base_commit,base_ref,git_dir"
            }
            "executions" => &execution_columns,
            "resource_leases" => "id,workspace_id,scope,pool,name,resource,reason,created_at,mode",
            _ => "*",
        };
        // A workspace's shared fields are its space's after the upgrade.
        let source = match table {
            "workspaces" if has_spaces(db) => "spaces JOIN workspaces USING(id) ORDER BY id",
            _ => &format!("{table} ORDER BY rowid"),
        };
        let mut statement = db.prepare(&format!("SELECT {columns} FROM {source}"))?;
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
            base_commit='abc', base_ref='refs/heads/main';
        UPDATE executions SET wrapper='{"pid":123,"birth":"wrapper"}',
            child='{"pid":456,"birth":"child"}', group_id=456;"#,
    )?;
    let before = ownership_snapshot(&mut old)?;
    super::tests::start_v0_17_0(&mut old)?;
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
                    ("executions_space", "space_id"),
                    ("spaces_repository", "repository_id"),
                ] {
                    let columns = db
                        .prepare(&format!("PRAGMA index_info({index})"))?
                        .query_map([], |row| row.get::<_, String>(2))?
                        .collect::<rusqlite::Result<Vec<_>>>()?;
                    assert_eq!(columns, [column]);
                }
                assert_eq!(
                    executions(db, &SpaceId::from("owner-0"))?[0].state,
                    ExecutionState::Running
                );
                assert_eq!(ports(db, Some(&SpaceId::from("owner-0")))?.len(), 2);
                let leases: i64 = db.query_row(
                    "SELECT COUNT(*) FROM resource_leases WHERE workspace_id='owner-0'",
                    [],
                    |row| row.get(0),
                )?;
                assert_eq!(leases, 2);
                Ok(())
            })
            .await?;
    }
    Ok(())
}
