//! Migration 316: each alert's attached paths move into their table.
use super::*;

/// A database at the floor with an alert `id` that attaches `attachments`.
fn with_alert(id: i64, attachments: &str) -> Result<Connection> {
    let db = at_floor()?;
    db.execute(
        "INSERT INTO alerts(id,created_at,kind,sender,message,attachments)
            VALUES (?1,0,'message','user','see',?2)",
        rusqlite::params![id, attachments],
    )?;
    Ok(db)
}

#[test]
fn an_alert_s_paths_move_into_their_table_in_order() -> Result<()> {
    let mut db = with_alert(3, r#"["/files/b.png","/files/a.png"]"#)?;
    db.execute(
        "INSERT INTO alerts(id,created_at,kind,sender,message) VALUES (4,0,'message','user','hi')",
        [],
    )?;
    migrate(&mut db)?;
    assert_eq!(
        rows(&db, "alert_attachments")?,
        ["(3, 0, '/files/b.png')", "(3, 1, '/files/a.png')"]
    );
    Ok(())
}

#[test]
fn an_alert_that_attaches_no_list_of_paths_fails_the_migration() -> Result<()> {
    let mut db = with_alert(3, r#"{"path":"/files/a.png"}"#)?;
    let error = format!("{:#}", migrate(&mut db).unwrap_err());
    assert!(error.contains("alerts row 3"), "{error}");
    Ok(())
}
