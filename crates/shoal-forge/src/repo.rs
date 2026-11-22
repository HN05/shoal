//! A repository's push access, forks and default branch.
pub mod default_branch;
pub mod fork;

use anyhow::Result;
use serde_json::Value;

use crate::transport::account::Account;
use shoal_forge_types::{
    ForgeRepo,
    backends::{ForgeKind, Routes},
};

/// How the forge tells who may push to a repository, forks it and reports
/// its default branch.
#[shoal_forge_derive::forwarded(ForgeKind, dispatch)]
pub(crate) trait Repositories: Routes {
    /// Whether `account` may push branches to the repository.
    async fn pushable(&self, repo: &ForgeRepo, account: &Account) -> Result<bool>;

    /// `owner/repository` of `account`'s fork of the repository, which the
    /// forge creates when the account has none.
    async fn fork(&self, repo: &ForgeRepo, account: &Account) -> Result<String>;

    /// The default branch a read of the repository's own endpoint returned
    /// as `value`.
    fn default_branch<'a>(&self, value: &'a Value) -> Option<&'a str> {
        value["default_branch"].as_str()
    }
}

#[cfg(test)]
mod tests;
