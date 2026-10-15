//! Open issues and PRs, for choosing one interactively.
use anyhow::{Context, Result};

use super::{ForgeKind, ForgeRepo, Query, account::Account, link::ItemKind};

/// An open issue or PR.
#[derive(Debug, PartialEq, serde::Deserialize)]
pub(crate) struct Item {
    pub number: u64,
    pub title: String,
}

impl ForgeRepo {
    pub async fn open_issues(&self, path: &std::path::Path) -> Result<Vec<Item>> {
        self.kind.open_items(self, path, "issue").await
    }

    pub async fn open_pulls(&self, path: &std::path::Path) -> Result<Vec<Item>> {
        self.kind.open_items(self, path, "pr").await
    }
}

impl ForgeKind {
    /// `noun` is the `issue` or `pr` subcommand of `gh`.
    async fn open_items(
        self,
        repo: &ForgeRepo,
        path: &std::path::Path,
        noun: &str,
    ) -> Result<Vec<Item>> {
        match self {
            Self::GitHub => {
                let repo = format!("{}/{}", repo.host, repo.path);
                let args = [
                    noun,
                    "list",
                    "--repo",
                    &repo,
                    "--state",
                    "open",
                    "--limit",
                    "200",
                    "--json",
                    "number,title",
                ];
                let text = self.query(path, &args, Query::List).await?;
                serde_json::from_str(&text)
                    .with_context(|| format!("invalid gh {noun} list response"))
            }
            Self::Forgejo => {
                let kind = if noun == "pr" {
                    ItemKind::Pr
                } else {
                    ItemKind::Issue
                };
                repo.rest_open_items(&Account::user(), kind)
                    .await
                    .context(super::api::FORGEJO_LOGIN)
            }
        }
    }
}
