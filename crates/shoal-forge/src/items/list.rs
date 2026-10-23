//! Open issues and PRs, for choosing one interactively.
use anyhow::Result;

use crate::{ForgeRepo, account::Account, backends::Backend, items::link::ItemKind};

/// An open issue or PR.
#[derive(Debug, PartialEq, serde::Deserialize)]
pub struct Item {
    pub number: u64,
    pub title: String,
}

impl ForgeRepo {
    pub async fn open_issues(&self, account: &Account) -> Result<Vec<Item>> {
        self.kind.open_items(self, account, ItemKind::Issue).await
    }

    pub async fn open_pulls(&self, account: &Account) -> Result<Vec<Item>> {
        self.kind.open_items(self, account, ItemKind::Pr).await
    }
}
