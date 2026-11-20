//! What apps use Shoal through: the connection to the daemon, an operation
//! for each request, the handles that read from a connection, and what the
//! `shoal` command prints. The types the protocol carries are
//! `shoal-protocol`'s.

pub mod agent_updates;
pub mod agents;
pub mod alerts;
pub mod approvals;
pub mod apps;
pub mod assignments;
pub mod attaching;
pub mod branches;
pub mod chats;
pub mod ci;
pub mod connection;
pub mod conversions;
pub mod demos;
pub mod diffs;
pub mod events;
pub mod executions;
pub mod files;
pub mod forge_reads;
pub mod hub;
pub mod items;
pub mod launches;
pub mod leases;
pub mod log;
pub mod operations;
pub mod output;
pub mod preparation;
pub mod priority;
pub mod progress;
pub mod queue;
pub mod recovery;
pub mod removal;
pub mod repositories;
pub mod reviews;
pub mod saved;
pub mod sleep;
pub mod spaces;
pub mod stopping;
pub mod subscriptions;
#[cfg(test)]
mod test_daemon;
pub mod threads;
pub mod transcripts;
pub mod transfer;
pub mod triggers;
pub mod views;
pub mod workspaces;

pub use shoal_forge_types as forge;

#[cfg(test)]
shoal_core::prepare_test_process!();
