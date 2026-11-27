//! Migrations 332 and 333: a completion's cleanup error in a column.
use super::*;

#[test]
fn a_completion_s_error_moves_into_its_column_and_journals_nothing() -> Result<()> {
    let mut db = at_floor()?;
    db.execute_batch(
        r#"INSERT INTO space_completion(space_id,record) VALUES
            ('failed','{"head":"h","cleanup":true,"error":"gone"}'),
            ('done','{"head":"h","cleanup":true,"error":null}');"#,
    )?;
    let events = |db: &Connection| -> rusqlite::Result<i64> {
        db.query_row("SELECT COUNT(*) FROM space_events", [], |row| row.get(0))
    };
    let before = events(&db)?;
    migrate(&mut db)?;
    let mut statement =
        db.prepare("SELECT space_id,record,error FROM space_completion ORDER BY space_id")?;
    let rows = statement
        .query_map([], |row| {
            Ok(format!(
                "{} {} {:?}",
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, Option<String>>(2)?,
            ))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    assert_eq!(
        rows,
        [
            r#"done {"cleanup":true,"head":"h"} None"#,
            r#"failed {"cleanup":true,"head":"h"} Some("gone")"#,
        ]
    );
    assert_eq!(events(&db)?, before);
    Ok(())
}

#[test]
fn a_completion_whose_error_is_no_text_is_refused() -> Result<()> {
    let mut db = at_floor()?;
    db.execute(
        r#"INSERT INTO space_completion(space_id,record) VALUES ('s','{"error":1}')"#,
        [],
    )?;
    let error = migrate(&mut db).unwrap_err();
    assert!(
        format!("{error:#}").contains("space_completion row s"),
        "{error:#}"
    );
    Ok(())
}
