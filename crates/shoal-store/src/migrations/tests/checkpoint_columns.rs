//! Migration 329: checkpoint records without the fields their columns keep.
use super::*;

const REPOSITORY: &str =
    "INSERT INTO repositories(id,path,source,last_used) VALUES ('repo','/repo','/repo',1)";

/// A checkpoint of the workspace `work` numbered 1 whose record is `record`.
fn at_floor_with(record: &str) -> Result<Connection> {
    let db = at_floor()?;
    db.execute(REPOSITORY, [])?;
    db.execute(
        "INSERT INTO checkpoints(workspace_id,number,repository_id,expires_at,record)
            VALUES ('work',1,'repo',20,?1)",
        [record],
    )?;
    Ok(db)
}

#[test]
fn a_checkpoint_record_loses_the_fields_its_columns_keep() -> Result<()> {
    let mut db = at_floor_with(
        r#"{"workspace_id":"work","repository_id":"repo","number":1,"cause":"manual",
            "branch":null,"head":"h","commit":"c","files_omitted":false,"created_at":10,
            "expires_at":20}"#,
    )?;
    migrate(&mut db)?;
    let record: String = db.query_row("SELECT record FROM checkpoints", [], |row| row.get(0))?;
    assert_eq!(
        record,
        r#"{"branch":null,"cause":"manual","commit":"c","created_at":10,"files_omitted":false,"head":"h"}"#
    );
    Ok(())
}

#[test]
fn a_checkpoint_whose_record_and_column_differ_is_refused() -> Result<()> {
    let mut db = at_floor_with(
        r#"{"workspace_id":"work","repository_id":"repo","number":1,"expires_at":30}"#,
    )?;
    let error = migrate(&mut db).unwrap_err();
    assert!(
        format!("{error:#}").contains("checkpoints row work/1"),
        "{error:#}"
    );
    Ok(())
}

#[test]
fn a_checkpoint_record_that_is_no_object_is_refused() -> Result<()> {
    let mut db = at_floor_with("[]")?;
    assert!(migrate(&mut db).is_err());
    Ok(())
}
