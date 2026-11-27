//! Migrations 335 and 336: a move destination's kind in a column.
use super::*;

fn at_floor_with(destination: &str) -> Result<Connection> {
    let db = at_floor()?;
    db.execute_batch(
        "INSERT INTO repositories(id,path,source,last_used) VALUES ('repo','/repo','/repo',1);
        BEGIN;
        INSERT INTO workspaces(id,path,branch,state,base_commit,git_dir)
            VALUES ('w','/w','w','ready','abc','/admin');
        INSERT INTO spaces(id,kind,repository_id,name) VALUES ('w','workspace','repo','w');
        COMMIT;",
    )?;
    db.execute(
        "INSERT INTO space_moves(space_id,destination,ticket,carrying,unsaved,phase,created_at,
            held_until) VALUES ('w',?1,'t','{}','{}','packing',1,9)",
        [destination],
    )?;
    Ok(db)
}

#[test]
fn a_move_s_destination_kind_moves_into_its_column() -> Result<()> {
    let mut db = at_floor_with(r#"{"kind":"ssh","host":"far","resume":true}"#)?;
    migrate(&mut db)?;
    let row: (String, String, Option<i64>) = db.query_row(
        "SELECT destination,destination_kind,held_until FROM space_moves",
        [],
        |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
    )?;
    assert_eq!(
        row,
        (
            r#"{"host":"far","resume":true}"#.into(),
            "ssh".into(),
            Some(9)
        )
    );
    Ok(())
}

#[test]
fn a_move_whose_destination_has_no_kind_is_refused() -> Result<()> {
    let mut db = at_floor_with(r#"{"path":"/p"}"#)?;
    let error = migrate(&mut db).unwrap_err();
    assert!(
        format!("{error:#}").contains("space_moves row w lacks kind"),
        "{error:#}"
    );
    Ok(())
}

#[test]
fn a_move_to_an_unknown_kind_of_destination_is_refused() -> Result<()> {
    let mut db = at_floor_with(r#"{"kind":"other"}"#)?;
    assert!(migrate(&mut db).is_err());
    Ok(())
}
