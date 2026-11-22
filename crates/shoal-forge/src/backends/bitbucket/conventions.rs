//! Bitbucket's description templates, labels and milestones.
use serde_json::Value;

use super::Bitbucket;
use crate::{
    conventions::{Conventions, Lookup},
    items::item::text,
    transport::api::TransportRequests,
};
use shoal_forge_types::{ForgeRepo, items::link::ItemKind};

impl Conventions for Bitbucket {
    fn template_lookups(&self, _kind: ItemKind) -> Vec<(&'static str, Lookup)> {
        Vec::new()
    }

    fn milestones_endpoint(&self, repo: &ForgeRepo) -> String {
        repo.repo_endpoint("milestones")
    }

    /// Bitbucket names a milestone, and issue edits take it by name.
    fn milestone(&self, listed: &Value) -> Option<(String, u64)> {
        Some((text(&listed["name"])?, listed["id"].as_u64()?))
    }
}
