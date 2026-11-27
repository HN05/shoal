//! Migration 331: removal records without the fields their columns keep.
use super::*;

/// The floor's database with a removal record of each ID, kind and JSON.
fn at_floor_with(records: &[(&str, bool, &str)]) -> Result<Connection> {
    let db = at_floor()?;
    db.execute(
        "INSERT INTO repositories(id,path,source,last_used) VALUES ('repo','/repo','/repo',1)",
        [],
    )?;
    for (id, chat, record) in records {
        db.execute(
            "INSERT INTO removed_spaces(space_id,repository_id,chat,name,expires_at,record)
                VALUES (?1,'repo',?2,?1,20,?3)",
            rusqlite::params![id, chat, record],
        )?;
    }
    Ok(db)
}

#[test]
fn removal_records_move_their_queried_fields_into_columns() -> Result<()> {
    let mut db = at_floor_with(&[
        (
            "chat",
            true,
            r#"{"workspace_id":"chat","repository_id":"repo","name":"chat","expires_at":20,
                "chat":{"mode":"chat","workspace":{"id":"work","name":"fix"}}}"#,
        ),
        (
            "home",
            true,
            r#"{"workspace_id":"home","repository_id":"repo","name":"home","expires_at":20,
                "chat":{"mode":"chat"}}"#,
        ),
        (
            "work",
            false,
            r#"{"workspace_id":"work","repository_id":"repo","name":"work","expires_at":20,
                "checkpoint":{"workspace_id":"work","number":1,"expires_at":20},"branch":"fix"}"#,
        ),
    ])?;
    migrate(&mut db)?;
    let mut statement = db.prepare(
        "SELECT space_id,chat_workspace_id,checkpoint_expires_at,record FROM removed_spaces
            ORDER BY space_id",
    )?;
    let rows = statement
        .query_map([], |row| {
            Ok(format!(
                "{} {:?} {:?} {}",
                row.get::<_, String>(0)?,
                row.get::<_, Option<String>>(1)?,
                row.get::<_, Option<i64>>(2)?,
                row.get::<_, String>(3)?,
            ))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    assert_eq!(
        rows,
        [
            r#"chat Some("work") None {"chat":{"mode":"chat","workspace":{"name":"fix"}}}"#,
            r#"home None None {"chat":{"mode":"chat"}}"#,
            r#"work None Some(20) {"branch":"fix","checkpoint":{"number":1,"workspace_id":"work"}}"#,
        ]
    );
    Ok(())
}

#[test]
fn a_workspace_s_record_without_its_checkpoint_s_expiry_is_refused() -> Result<()> {
    let mut db = at_floor_with(&[(
        "work",
        false,
        r#"{"workspace_id":"work","repository_id":"repo","name":"work","expires_at":20,
            "checkpoint":{"number":1}}"#,
    )])?;
    let error = migrate(&mut db).unwrap_err();
    let error = format!("{error:#}");
    assert!(
        error.contains("removed_spaces row work lacks checkpoint.expires_at"),
        "{error}"
    );
    Ok(())
}

#[test]
fn a_record_whose_name_differs_from_its_column_is_refused() -> Result<()> {
    let mut db = at_floor_with(&[(
        "home",
        true,
        r#"{"workspace_id":"home","repository_id":"repo","name":"other","expires_at":20,
            "chat":{"mode":"chat"}}"#,
    )])?;
    assert!(migrate(&mut db).is_err());
    Ok(())
}
