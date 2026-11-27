//! The daemon's SQLite store, its schema and migrations, and the record
//! functions every subsystem shares.
pub mod agent_handoffs;
mod executions;
mod guards;
pub mod journal;
pub mod lifecycle;
mod migrations;
mod ports;
mod rows;
pub mod spaces;
mod sql;
mod store;
#[cfg(any(test, feature = "test-support"))]
pub mod test_support;
pub mod tree;

pub use executions::{
    AgentExecution, EXECUTION_COLUMNS, agent_executions, agent_wrappers, all_executions, execution,
    executions, find_execution,
};
pub use guards::{
    ensure_no_pending_move, ensure_no_pending_rename, ensure_path_unrecorded,
    ensure_repository_not_moving, not_ready, repository_moving, repository_removing, require_ready,
    require_ready_space,
};
pub use migrations::SCHEMA_VERSION;
pub use ports::ports;
pub use rows::{REPOSITORY_COLUMNS, WORKSPACE_COLUMNS, repository, workspace};
pub use sql::{exists, json_column, json_text, narrowing_column};
pub use store::{Store, write_transaction};
