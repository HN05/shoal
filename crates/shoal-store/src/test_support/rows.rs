//! A database with a row of each record the store's mappers read, and the
//! check that a mapper reads its columns by name.
use anyhow::Result;
use rusqlite::{Connection, Row};
use serde::Serialize;

/// A database with the current schema holding the repository `repo`, its
/// workspace `workspace`, that workspace's port `web` and its execution
/// `execution`.
pub fn mapped_rows() -> Result<Connection> {
    let db = super::migrated()?;
    db.execute_batch(
        r#"INSERT INTO repositories(id,path,source,last_used,name,workspaces_dir)
                VALUES ('repo','/checkout','origin',42,'project','/workspaces');
            INSERT INTO spaces(id,kind,repository_id,name)
                VALUES ('workspace','workspace','repo','worker');
            INSERT INTO workspaces(id,path,branch,state,error,
                base_commit,base_ref,git_dir,setup_finished)
                VALUES ('workspace','/work','feature','failed','setup error',
                    'abc','refs/heads/main','/git/worktrees/worker',1);
            INSERT INTO ports(workspace_id,name,port,env_var,reason)
                VALUES ('workspace','web',12345,'WEB_PORT',NULL);
            INSERT INTO executions(id,space_id,state,wrapper,child,group_id,agent)
                VALUES ('execution','workspace','unknown','{"pid":123,"birth":"wrapper"}',
                    '{"pid":456,"birth":"child"}',456,'claude');"#,
    )?;
    Ok(db)
}

/// Check that `mapper` reads the one row of `table` as `expected` from
/// `columns`, and from them reordered after an unrelated column.
pub fn check_columns<T: Serialize>(
    db: &Connection,
    table: &str,
    columns: &str,
    mapper: fn(&Row<'_>) -> rusqlite::Result<T>,
    expected: serde_json::Value,
) -> Result<()> {
    // Exercise the shared projection and a reordered projection with an
    // unrelated leading column, as a changed schema or query might supply.
    let reordered = format!(
        "'extra' AS unrelated,{}",
        columns.split(',').rev().collect::<Vec<_>>().join(",")
    );
    for projection in [columns, &reordered] {
        let record = db.query_row(&format!("SELECT {projection} FROM {table}"), [], mapper)?;
        assert_eq!(serde_json::to_value(record)?, expected);
    }
    Ok(())
}
