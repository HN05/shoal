use super::*;
use rusqlite::params_from_iter;

/// The plan a row with the column `values` holds, as JSON.
fn read_back(values: [SqlValue; 3]) -> Result<serde_json::Value> {
    let db = Connection::open_in_memory()?;
    let plan = db.query_row(
        "SELECT ?1 AS pending_plan, ?2 AS pending_source, ?3 AS pending_checkpoint",
        params_from_iter(values),
        plan,
    )?;
    Ok(serde_json::to_value(plan)?)
}

#[test]
fn a_restore_s_kind_and_checkpoint_go_in_columns() -> Result<()> {
    let plan = PendingWorktree::restore(4.into());
    let values = values(Some(&plan))?;
    assert_eq!(
        values,
        [
            SqlValue::Text(r#"{"source":{}}"#.into()),
            SqlValue::Text("restore".into()),
            SqlValue::Integer(4),
        ]
    );
    assert_eq!(read_back(values)?, serde_json::to_value(&plan)?);
    Ok(())
}

#[test]
fn a_copy_of_live_files_names_no_checkpoint() -> Result<()> {
    let plan = PendingWorktree::copy("source".into(), None, Some("work"));
    let values = values(Some(&plan))?;
    assert_eq!(values[2], SqlValue::Null);
    assert_eq!(read_back(values)?, serde_json::to_value(&plan)?);
    Ok(())
}

#[test]
fn a_created_worktree_has_no_plan() -> Result<()> {
    assert_eq!(read_back(values(None)?)?, serde_json::Value::Null);
    Ok(())
}
