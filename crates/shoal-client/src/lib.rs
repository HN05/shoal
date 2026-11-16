//! What apps use Shoal through: the daemon's protocol, the types it carries,
//! and the client-side logic the `shoal` command shares with them.

pub mod agent_messages;
pub mod agents;
pub mod approvals;
pub mod assignments;
pub mod chats;
pub mod ci;
pub mod connection;
pub mod conversions;
pub mod demos;
pub mod events;
pub mod executions;
pub mod files;
pub mod forge_reads;
pub mod items;
pub mod leases;
pub mod log;
pub mod notifications;
pub mod operations;
pub mod output;
pub mod preparation;
pub mod priority;
pub mod progress;
pub mod questions;
pub mod queue;
pub mod recovery;
pub mod removal;
pub mod repositories;
pub mod restarts;
pub mod reviews;
pub mod saved;
pub mod sleep;
pub mod stopping;
pub mod subscriptions;
pub mod transcripts;
pub mod transfer;
pub mod views;
pub mod workspaces;

pub use shoal_forge_types as forge;
