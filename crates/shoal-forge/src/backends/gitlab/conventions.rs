//! GitLab's description templates, labels and milestones.
use serde_json::Value;

use super::GitLab;
use crate::{ForgeRepo, backends::Conventions, conventions::Lookup, items::link::ItemKind};

impl Conventions for GitLab {
    fn template_lookups(&self, kind: ItemKind) -> Vec<(&'static str, Lookup)> {
        match kind {
            ItemKind::Pr => vec![(
                ".gitlab/merge_request_templates",
                Lookup::File("default.md"),
            )],
            ItemKind::Issue => vec![(".gitlab", Lookup::Directory("issue_templates"))],
        }
    }

    /// GitLab's scoped labels are named `key::value`.
    fn exclusive_label(&self, name: &str, _label: &Value) -> bool {
        name.contains("::")
    }

    /// The project's milestones and those of the groups above it, which
    /// lists both open and closed ones unless asked for one state.
    fn milestones_endpoint(&self, repo: &ForgeRepo) -> String {
        repo.repo_endpoint("milestones?include_ancestors=true")
    }
}
