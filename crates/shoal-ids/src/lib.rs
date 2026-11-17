//! The IDs the daemon gives its records and the numbers that count them,
//! which every Shoal crate shares, so one kind passed where another belongs
//! does not compile.

pub mod agent_session_ids;
pub mod checkpoint_numbers;
pub mod execution_ids;
mod macros;
pub mod member_ids;
pub mod queue_entry_ids;
pub mod repository_ids;
pub mod space_ids;
