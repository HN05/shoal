use anyhow::Result;

use super::*;
use crate::test_support::queries::{check_query_growth, populated_database};
use crate::test_support::rows::{check_columns, mapped_rows};
use crate::test_support::{migrated, seeds};

#[test]
fn the_execution_mapper_uses_named_columns() -> Result<()> {
    check_columns(
        &mapped_rows()?,
        "executions",
        EXECUTION_COLUMNS,
        execution,
        serde_json::json!({
            "id": "execution", "space_id": "workspace", "state": "unknown",
            "wrapper": {"pid": 123, "birth": "wrapper"},
            "child": {"pid": 456, "birth": "child"}, "group_id": 456,
            "agent": "claude"
        }),
    )
}

#[test]
fn execution_owner_reads_search_their_index() -> Result<()> {
    let db = populated_database()?;
    check_query_growth(
        &db,
        &executions_query(),
        "executions",
        "space_id",
        "owner-0",
    )
}

#[test]
fn each_execution_is_read_with_its_space() -> Result<()> {
    let db = migrated()?;
    let topic = seeds::ready_workspace(&db, "repo", "topic", "topic")?;
    let other = seeds::ready_workspace(&db, "repo", "other", "other")?;
    for (id, space) in [("first", &topic.id), ("second", &other.id)] {
        seeds::bare_execution(&db, id, space, ExecutionState::Running)?;
    }
    let read = execution_spaces(&db)?;
    assert_eq!(
        read,
        [("first".into(), topic.id), ("second".into(), other.id)]
    );
    Ok(())
}
