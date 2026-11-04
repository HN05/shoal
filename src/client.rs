//! What apps use Shoal through: the daemon's protocol, the types it carries,
//! and the client-side logic the CLI shares with them.

pub mod agent;
pub mod agent_messages;
pub mod ai;
pub mod approvals;
pub mod availability;
pub mod cleanup;
pub mod config;
pub mod connection;
pub mod demos;
pub mod env;
pub mod events;
pub mod executions;
pub mod files;
pub mod forge_reads;
pub(crate) mod fsutil;
pub mod item_numbers;
pub mod leases;
pub mod links;
pub mod list_status;
pub mod model;
pub mod notifications;
pub mod operations;
pub mod output;
pub mod paths;
pub mod processes;
pub(crate) mod profile;
pub mod protocol;
pub mod questions;
pub mod recovery;
pub mod removal;
pub mod repository;
pub mod resources;
pub mod reviews;
pub mod secrets;
pub mod sessions;
pub mod sim;
pub mod state;
pub mod subscriptions;
pub mod time;
pub mod transcripts;
pub mod transfer;
pub mod validate;
pub mod views;

pub use shoal_forge as forge;
