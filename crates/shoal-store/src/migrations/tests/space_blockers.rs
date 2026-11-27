//! Migration 334: a plan's blockers in rows of their own.
use super::*;

fn at_floor_with(record: &str) -> Result<Connection> {
    let db = at_floor()?;
    db.execute(
        "INSERT INTO space_progress(space_id,record,updated_at) VALUES ('s',?1,1)",
        [record],
    )?;
    Ok(db)
}

#[test]
fn a_plan_s_blockers_move_into_rows_in_their_order() -> Result<()> {
    let mut db = at_floor_with(
        r#"{"status":"waiting","steps":[{"name":"build","state":"active","key":"b"}],
            "blockers":[
                {"kind":"issue","target":"u1","name":"u1","reason":"down"},
                {"kind":"pr","target":"u2","name":"u2","reason":"first","lookup_failed":"503",
                    "step":{"key":"b"}},
                {"kind":"space","target":"id","name":"api","reason":"needs it",
                    "step":{"name":"build"}}]}"#,
    )?;
    migrate(&mut db)?;
    let record: String = db.query_row("SELECT record FROM space_progress", [], |row| row.get(0))?;
    assert_eq!(
        record,
        r#"{"status":"waiting","steps":[{"key":"b","name":"build","state":"active"}]}"#
    );
    let mut statement = db.prepare(
        "SELECT position,kind,target,name,reason,lookup_failed,step_key,step_name
         FROM space_blockers ORDER BY position",
    )?;
    let rows = statement
        .query_map([], |row| {
            Ok(format!(
                "{} {} {} {} {} {:?} {:?} {:?}",
                row.get::<_, i64>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, String>(3)?,
                row.get::<_, String>(4)?,
                row.get::<_, Option<String>>(5)?,
                row.get::<_, Option<String>>(6)?,
                row.get::<_, Option<String>>(7)?,
            ))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    assert_eq!(
        rows,
        [
            "0 issue u1 u1 down None None None",
            r#"1 pr u2 u2 first Some("503") Some("b") None"#,
            r#"2 space id api needs it None None Some("build")"#,
        ]
    );
    Ok(())
}

#[test]
fn a_blocker_without_a_reason_is_refused() -> Result<()> {
    let mut db = at_floor_with(r#"{"blockers":[{"kind":"issue","target":"u","name":"u"}]}"#)?;
    let error = migrate(&mut db).unwrap_err();
    assert!(
        format!("{error:#}").contains("space_progress row s"),
        "{error:#}"
    );
    Ok(())
}
