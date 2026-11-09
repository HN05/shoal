//! Port reservations, requests and catalogs.
use super::Workspace;
use crate::config::repo::{ConflictPolicy, PortDefinition};
use crate::space_ids::SpaceId;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PortReservation {
    pub workspace_id: SpaceId,
    pub name: String,
    pub port: u16,
    pub env_var: String,
    pub reason: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PortSuggestion {
    pub workspace_id: SpaceId,
    pub name: String,
    pub requested_port: u16,
    pub suggested_port: u16,
    pub env_var: String,
    pub reason: Option<String>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct PortOverview {
    pub workspace: Workspace,
    pub reserved: Vec<PortReservation>,
}

/// The port configuration a workspace's reservations draw from.
#[derive(Debug, Serialize, Deserialize)]
pub struct PortCatalog {
    pub workspace: Workspace,
    pub start: u16,
    pub end: u16,
    pub on_conflict: ConflictPolicy,
    pub configured: std::collections::BTreeMap<String, PortDefinition>,
}

/// Caller overrides for a named port; unset fields fall back to the
/// repository's port definition.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct PortRequest {
    pub port: Option<u16>,
    pub env_var: Option<String>,
    pub reason: Option<String>,
    pub on_conflict: Option<ConflictPolicy>,
}
