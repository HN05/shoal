use super::*;
use rusqlite::types::Value;

/// The variable that has [`a_new_database_takes_the_schema_the_migrations_make`]
/// write [`SCHEMA`] anew from the migrations.
const WRITE_SCHEMA: &str = "SHOAL_WRITE_SCHEMA";

/// A database that has taken every migration in turn.
fn migrated() -> Result<Connection> {
    let mut db = Connection::open_in_memory()?;
    db.pragma_update(None, "foreign_keys", true)?;
    migrate_to(&mut db, SCHEMA_VERSION)?;
    Ok(db)
}

/// The statements that made `db`'s schema objects of `kind`, in the order
/// it made them.
fn statements(db: &Connection, kind: &str) -> Result<Vec<String>> {
    let mut statement = db.prepare(
        "SELECT sql FROM sqlite_schema WHERE type=?1 AND sql IS NOT NULL
            AND name NOT LIKE 'sqlite_%' ORDER BY rowid",
    )?;
    let rows = statement.query_map([kind], |row| row.get(0))?;
    Ok(rows.collect::<rusqlite::Result<_>>()?)
}

/// The names of `db`'s tables, SQLite's own included.
fn tables(db: &Connection) -> Result<Vec<String>> {
    let mut statement =
        db.prepare("SELECT name FROM sqlite_schema WHERE type='table' ORDER BY rowid")?;
    let rows = statement.query_map([], |row| row.get(0))?;
    Ok(rows.collect::<rusqlite::Result<_>>()?)
}

/// The rows of `table`, each as the SQL values that insert it.
fn rows(db: &Connection, table: &str) -> Result<Vec<String>> {
    let mut statement = db.prepare(&format!("SELECT * FROM \"{table}\""))?;
    let columns = statement.column_count();
    let rows = statement.query_map([], |row| {
        let values = (0..columns)
            .map(|column| row.get::<_, Value>(column).map(literal))
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(format!("({})", values.join(", ")))
    })?;
    Ok(rows.collect::<rusqlite::Result<_>>()?)
}

/// `value` as an SQL literal.
fn literal(value: Value) -> String {
    match value {
        Value::Null => "NULL".to_owned(),
        Value::Integer(number) => number.to_string(),
        Value::Real(number) => number.to_string(),
        Value::Text(text) => format!("'{}'", text.replace('\'', "''")),
        Value::Blob(bytes) => {
            let hex: String = bytes.iter().map(|byte| format!("{byte:02x}")).collect();
            format!("X'{hex}'")
        }
    }
}

/// The SQL that makes `db`'s schema with the rows it holds: tables first,
/// so that each index finds its table, then the rows, before the triggers
/// that inserting them would set off.
fn schema_of(db: &Connection) -> Result<String> {
    let mut sql = statements(db, "table")?;
    sql.extend(statements(db, "index")?);
    sql.extend(statements(db, "view")?);
    for table in tables(db)? {
        let inserts = rows(db, &table)?.into_iter();
        sql.extend(inserts.map(|row| format!("INSERT INTO \"{table}\" VALUES {row}")));
    }
    sql.extend(statements(db, "trigger")?);
    Ok(format!(
        "-- The schema the migrations in migrations.rs make, which a new database\n\
         -- takes. Written by\n\
         -- {WRITE_SCHEMA}=1 cargo nextest run -p shoal-store -E 'test(a_new_database_takes)'\n\n{};\n",
        sql.join(";\n\n")
    ))
}

/// Each schema object of `db`, SQLite's own included, with the statement
/// that made it and a table's rows, and the schema version.
type Contents = (Vec<(String, Option<String>, Vec<String>)>, i64);

fn contents(db: &Connection) -> Result<Contents> {
    let mut statement = db.prepare(
        "SELECT type || ' ' || name, sql, type='table', name FROM sqlite_schema
            ORDER BY type, name",
    )?;
    let objects = statement
        .query_map([], |row| {
            Ok((
                row.get(0)?,
                row.get(1)?,
                row.get::<_, bool>(2)?,
                row.get::<_, String>(3)?,
            ))
        })?
        .collect::<rusqlite::Result<Vec<(String, Option<String>, bool, String)>>>()?;
    let mut found = Vec::new();
    for (object, sql, table, name) in objects {
        let rows = if table { rows(db, &name)? } else { Vec::new() };
        found.push((object, sql, rows));
    }
    let version = db.query_row("PRAGMA user_version", [], |row| row.get(0))?;
    Ok((found, version))
}

#[test]
fn a_new_database_takes_the_schema_the_migrations_make() -> Result<()> {
    let migrated = migrated()?;
    if std::env::var_os(WRITE_SCHEMA).is_some() {
        // Read at run time: a test binary a workspace keeps from its seed
        // build was compiled in the seed's checkout.
        let package = std::env::var_os("CARGO_MANIFEST_DIR").context("no CARGO_MANIFEST_DIR")?;
        let path = std::path::Path::new(&package).join("src/schema.sql");
        std::fs::write(path, schema_of(&migrated)?)?;
    }
    let mut created = Connection::open_in_memory()?;
    created.pragma_update(None, "foreign_keys", true)?;
    migrate(&mut created)?;
    assert!(
        contents(&created)? == contents(&migrated)?,
        "schema.sql is not what the migrations make; write it anew with \
         {WRITE_SCHEMA}=1 cargo nextest run -p shoal-store -E 'test(a_new_database_takes)'"
    );
    Ok(())
}

#[test]
fn a_database_with_tables_but_no_version_takes_the_migrations() -> Result<()> {
    let mut db = Connection::open_in_memory()?;
    db.execute_batch(MIGRATIONS[0].1)?;
    migrate(&mut db)?;
    assert!(contents(&db)? == contents(&migrated()?)?);
    Ok(())
}
