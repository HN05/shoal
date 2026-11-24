//! What the origin of a registered checkout or worktree names, read with
//! the user's own logins: an open issue a new space is assigned, a PR's
//! title, and the issue a number or URL names there.
use std::path::Path;

use anyhow::{Context, Result};
use shoal_core::repository::remote_url_from_path;
use shoal_forge::{RepoRequests, items::issue::IssueRequests};
use shoal_forge_types::{
    ForgeRepo,
    items::{
        link::LinkKind,
        reference::{ItemInput, ItemRef},
    },
};

use crate::Links;

/// An open issue a new workspace is assigned.
#[derive(Debug, Clone)]
pub struct OpenIssue {
    pub url: ItemRef,
    pub title: String,
    pub labels: Vec<String>,
}

impl Links {
    /// The canonical URL, title and labels of the open issue `input` names in the
    /// repository whose origin `checkout` has, read with the user's logins,
    /// as `shoal start` reads it. A closed or unknown issue is refused.
    pub async fn read_open_issue(&self, checkout: &Path, input: &ItemInput) -> Result<OpenIssue> {
        let (forge, url) = self.issue_forge(checkout, input).await?;
        let issue = forge
            .open_issue_details(&self.forges.user_account(), url.issue()?)
            .await?;
        Ok(OpenIssue {
            url,
            title: issue.item.title,
            labels: issue.item.labels,
        })
    }

    /// The title of the PR `input` names in the repository whose origin
    /// `checkout` has.
    pub async fn read_pull_title(&self, checkout: &Path, input: &ItemInput) -> Result<String> {
        let remote = remote_url_from_path(checkout)
            .await?
            .context("PR lookup needs an origin remote")?;
        let forge = self.forges.forge(&remote).await?;
        let pull = ItemRef::in_repository(&forge, LinkKind::Pr, input)?;
        Ok(forge
            .pull_request(&self.forges.user_account(), &pull)
            .await?
            .title)
    }

    /// The forge of the repository whose origin `checkout` has, with the
    /// number and canonical URL of the issue `input` names there.
    pub async fn issue_forge(
        &self,
        checkout: &Path,
        input: &ItemInput,
    ) -> Result<(ForgeRepo, ItemRef)> {
        let remote = remote_url_from_path(checkout)
            .await?
            .context("issue lookup needs an origin remote")?;
        let forge = self.forges.forge(&remote).await?;
        let issue = ItemRef::in_repository(&forge, LinkKind::Issue, input)?;
        Ok((forge, issue))
    }
}
