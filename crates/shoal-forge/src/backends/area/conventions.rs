//! What each forge does its own way with description templates, labels and
//! milestones.
use anyhow::Result;
use serde_json::Value;

use crate::{
    ForgeRepo,
    account::Account,
    backends::{Backend, ForgeKind},
    conventions::Lookup,
    items::link::ItemKind,
};

/// The forge's description templates, labels and milestones.
#[shoal_forge_derive::forwarded(ForgeKind, dispatch)]
pub(crate) trait Conventions: Backend {
    /// Where the forge looks for the description templates of a new item
    /// of `kind`, in its lookup order.
    fn template_lookups(&self, kind: ItemKind) -> Vec<(&'static str, Lookup)>;

    /// Every label the repository's items can take, as the forge lists them.
    async fn labels(&self, repo: &ForgeRepo, account: &Account) -> Result<Vec<Value>> {
        repo.pages(account, &repo.repo_endpoint("labels")).await
    }

    /// Whether the label `name`, as the forge lists it in `label`, allows an
    /// item at most one label of its scope.
    fn exclusive_label(&self, _name: &str, _label: &Value) -> bool {
        false
    }

    /// Whether label names differing only in case name the same label.
    fn label_names_fold_case(&self) -> bool {
        false
    }

    /// The paginated list of the repository's milestones, open and closed.
    fn milestones_endpoint(&self, repo: &ForgeRepo) -> String {
        repo.repo_endpoint("milestones?state=all")
    }

    /// The title of a listed milestone and the ID edits take for it.
    fn milestone(&self, listed: &Value) -> Option<(String, u64)> {
        Some((listed["title"].as_str()?.to_owned(), listed["id"].as_u64()?))
    }
}
