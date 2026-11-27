//! Migration 318: the hub's space numbers and spares leave its JSON.
use super::*;

/// A database at the floor with the machines `desk` and `laptop`.
fn with_machines() -> Result<Connection> {
    let db = at_floor()?;
    db.execute_batch(
        "INSERT INTO hub_members(id,kind,name,public_key,joined_at)
            VALUES ('desk','machine','desk','k1',1), ('laptop','machine','laptop','k2',1);",
    )?;
    Ok(db)
}

#[test]
fn listed_spaces_and_copies_keep_their_numbers_in_columns() -> Result<()> {
    let mut db = with_machines()?;
    db.execute_batch(
        r#"INSERT INTO hub_spaces(space_id,member_id,summary,reported_at)
            VALUES ('w1','desk','{"id":"w1","number":4,"name":"work"}',10);
        INSERT INTO hub_space_copies(space_id,member_id,summary,since,handed_over)
            VALUES ('w1','laptop','{"id":"w1","number":6,"name":"copy"}',12,1);"#,
    )?;
    migrate(&mut db)?;
    assert_eq!(
        rows(&db, "hub_spaces")?,
        [r#"('w1', 'desk', 4, '{"name":"work"}', 10)"#]
    );
    assert_eq!(
        rows(&db, "hub_space_copies")?,
        [r#"('w1', 'laptop', 6, '{"name":"copy"}', 12, 1)"#]
    );
    assert_eq!(rows(&db, "hub_held_numbers")?, ["('laptop', 6, 'w1')"]);
    Ok(())
}

#[test]
fn each_numbering_keeps_its_spares_in_a_table() -> Result<()> {
    let mut db = at_floor()?;
    db.execute_batch(
        "INSERT INTO hub_numbering(id,spares) VALUES (1,'[5,3]');
        INSERT INTO hub_alert_numbering(id,spares,taken,seen) VALUES (1,'[112,111]',100,105);",
    )?;
    migrate(&mut db)?;
    assert_eq!(rows(&db, "hub_numbering")?, ["(1)"]);
    assert_eq!(rows(&db, "hub_numbering_spares")?, ["(3)", "(5)"]);
    assert_eq!(rows(&db, "hub_alert_numbering")?, ["(1, 100, 105)"]);
    assert_eq!(rows(&db, "hub_alert_numbering_spares")?, ["(111)", "(112)"]);
    Ok(())
}

#[test]
fn a_summary_without_its_number_fails_the_migration() -> Result<()> {
    let mut db = with_machines()?;
    db.execute_batch(
        r#"INSERT INTO hub_spaces(space_id,member_id,summary,reported_at)
            VALUES ('w1','desk','{"id":"w1","name":"work"}',10);"#,
    )?;
    let error = format!("{:#}", migrate(&mut db).unwrap_err());
    assert!(error.contains("hub_spaces row 1, of space w1"), "{error}");
    Ok(())
}
