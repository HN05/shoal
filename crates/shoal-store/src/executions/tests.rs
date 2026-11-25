use anyhow::Result;

use super::*;
use crate::test_support::queries::{check_query_growth, populated_database};
use crate::test_support::rows::{check_columns, mapped_rows};

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
