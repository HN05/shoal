//! Migration 330: a pending plan's source kind and checkpoint in columns.
use super::*;

/// The floor's database with a pending workspace of each ID and plan.
fn at_floor_with(plans: &[(&str, &str)]) -> Result<Connection> {
    let db = at_floor()?;
    db.execute(
        "INSERT INTO repositories(id,path,source,last_used) VALUES ('repo','/repo','/repo',1)",
        [],
    )?;
    for (id, plan) in plans {
        db.execute_batch("BEGIN")?;
        db.execute(
            "INSERT INTO workspaces(id,path,branch,state,pending_plan)
                VALUES (?1,?1,?1,'preparing',?2)",
            [id, plan],
        )?;
        db.execute(
            "INSERT INTO spaces(id,kind,repository_id,name) VALUES (?1,'workspace','repo',?1)",
            [id],
        )?;
        db.execute_batch("COMMIT")?;
    }
    Ok(db)
}

#[test]
fn a_plan_s_source_kind_and_checkpoint_move_into_columns() -> Result<()> {
    let mut db = at_floor_with(&[
        (
            "restore",
            r#"{"source":{"kind":"restore","checkpoint":4,"base":"main"}}"#,
        ),
        (
            "copy",
            r#"{"source":{"kind":"copy","workspace":"w"},"git_profile":"work"}"#,
        ),
    ])?;
    db.execute_batch(
        "BEGIN;
        INSERT INTO workspaces(id,path,branch,state,base_commit,git_dir)
            VALUES ('created','/created','created','ready','abc','/admin');
        INSERT INTO spaces(id,kind,repository_id,name)
            VALUES ('created','workspace','repo','created');
        COMMIT;",
    )?;
    migrate(&mut db)?;
    let mut statement = db.prepare(
        "SELECT id,pending_plan,pending_source,pending_checkpoint FROM workspaces ORDER BY id",
    )?;
    let rows = statement
        .query_map([], |row| {
            Ok(format!(
                "{} {:?} {:?} {:?}",
                row.get::<_, String>(0)?,
                row.get::<_, Option<String>>(1)?,
                row.get::<_, Option<String>>(2)?,
                row.get::<_, Option<i64>>(3)?,
            ))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    assert_eq!(
        rows,
        [
            r#"copy Some("{\"git_profile\":\"work\",\"source\":{\"workspace\":\"w\"}}") Some("copy") None"#,
            "created None None None",
            r#"restore Some("{\"source\":{\"base\":\"main\"}}") Some("restore") Some(4)"#,
        ]
    );
    Ok(())
}

#[test]
fn a_plan_without_a_source_kind_is_refused() -> Result<()> {
    let mut db = at_floor_with(&[("w", r#"{"source":{}}"#)])?;
    let error = migrate(&mut db).unwrap_err();
    assert!(
        format!("{error:#}").contains("workspaces row w lacks source.kind"),
        "{error:#}"
    );
    Ok(())
}

#[test]
fn a_plan_whose_checkpoint_is_no_number_is_refused() -> Result<()> {
    let mut db = at_floor_with(&[("w", r#"{"source":{"kind":"restore","checkpoint":"4"}}"#)])?;
    assert!(migrate(&mut db).is_err());
    Ok(())
}

#[test]
fn a_plan_of_an_unknown_source_kind_is_refused() -> Result<()> {
    let mut db = at_floor_with(&[("w", r#"{"source":{"kind":"other"}}"#)])?;
    let error = migrate(&mut db).unwrap_err();
    assert!(
        format!("{error:#}").contains("source.kind holds no value it takes"),
        "{error:#}"
    );
    Ok(())
}
