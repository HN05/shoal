//! GitHub's description templates, labels and milestones.
use serde_json::Value;

use super::GitHub;
use crate::{
    backends::Conventions,
    conventions::{Lookup, shared_lookups},
    items::{item::text, link::ItemKind},
};

impl Conventions for GitHub {
    fn template_lookups(&self, kind: ItemKind) -> Vec<(&'static str, Lookup)> {
        shared_lookups(kind)
    }

    /// GitHub keeps its own spelling of a label added in another case.
    fn label_names_fold_case(&self) -> bool {
        true
    }

    /// GitHub's issue edits take a milestone's number.
    fn milestone(&self, listed: &Value) -> Option<(String, u64)> {
        Some((text(&listed["title"])?, listed["number"].as_u64()?))
    }
}
