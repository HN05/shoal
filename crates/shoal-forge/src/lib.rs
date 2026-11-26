//! Requests to the forge a Git remote names. Callers choose the
//! [`transport::account::Account`] a request acts as; this crate never
//! decides who may act.

/// Run `$body` with `$backend` bound to the backend `$kind` names. The
/// traits of what forges do their own way forward their methods through it,
/// so it comes before the modules that declare them.
macro_rules! dispatch {
    ($kind:expr, $backend:ident => $body:expr) => {
        match $kind {
            ::shoal_forge_types::backends::ForgeKind::GitHub => {
                let $backend = ::shoal_forge_types::backends::github::GitHub;
                $body
            }
            ::shoal_forge_types::backends::ForgeKind::Forgejo => {
                let $backend = ::shoal_forge_types::backends::forgejo::Forgejo;
                $body
            }
            ::shoal_forge_types::backends::ForgeKind::GitLab => {
                let $backend = ::shoal_forge_types::backends::gitlab::GitLab;
                $body
            }
            ::shoal_forge_types::backends::ForgeKind::Bitbucket => {
                let $backend = ::shoal_forge_types::backends::bitbucket::Bitbucket;
                $body
            }
        }
    };
}

pub mod activity;
pub mod backends;
pub mod branches;
pub mod capability;
pub mod ci;
pub mod clearance;
pub mod conventions;
pub mod download;
pub mod hosts;
pub mod items;
pub mod raw;
pub mod repo;
#[cfg(any(test, feature = "test-support"))]
pub mod test_support;
pub mod transport;
pub mod updates;

use anyhow::{Context, Result, ensure};
use shoal_forge_types::{
    ForgeRepo, PullRequest,
    items::{item::Current, reference::ItemRef},
    numbers::{IssueNumber, PullNumber},
    repo::remote_url::{self, RemoteUrl},
};
use shoal_refs::CommitId;

use crate::{
    hosts::{HostType, HostTypes},
    items::{Items, merging::Merging},
    transport::account::Account,
};

/// Reading a repository's PRs and resolving the forge a host serves.
#[shoal_forge_derive::extension(pub RepoRequests, pub(crate) RepoSteps)]
impl ForgeRepo {
    /// The repository `remote` names, on the forge `hosts` says its host
    /// serves, which this process then remembers when the host answered it.
    pub async fn resolve(remote: &str, hosts: &impl HostTypes) -> Result<Self> {
        let remote = RemoteUrl::parse(remote).context("issue lookup needs a forge remote")?;
        let (host, path, web_scheme) = Self::location(&remote)?;
        let resolved = hosts.forge_type(&host, web_scheme).await?;
        if let HostType::Answered(kind) = resolved {
            shoal_forge_types::hosts::learn(&host, kind);
        }
        let kind = resolved.kind();
        Self::on(kind, host, path, web_scheme)
    }

    /// The repository `input` names: a remote or web URL, on any forge, or
    /// an `owner/repository` path on this repository's host.
    pub async fn resolve_named(&self, input: &str, hosts: &impl HostTypes) -> Result<Self> {
        if Self::named_by_url(input) {
            return Self::resolve(input, hosts).await;
        }
        let path = remote_url::repository_path(input);
        Self::on(self.kind(), self.host.clone(), path, self.web_scheme())
            .with_context(|| format!("{input} names no repository"))
    }

    /// The issue `number`, open or closed, which must have a title.
    pub async fn issue_details(&self, account: &Account, number: IssueNumber) -> Result<Current> {
        let issue = self.kind().issue_details(self, account, number).await?;
        ensure!(!issue.item.title.trim().is_empty(), "issue title is empty");
        Ok(issue)
    }

    /// Fail closed on changed output. Only commits actually listed in the
    /// merged PR can authorize cleanup, including squash/rebase merges.
    pub async fn merged_commits(
        &self,
        account: &Account,
        number: PullNumber,
    ) -> Result<Option<Vec<CommitId>>> {
        self.kind().merged_commits(self, account, number).await
    }

    /// The PR `pull`, of this repository, with where its head is.
    pub async fn pull_request(&self, account: &Account, pull: &ItemRef) -> Result<PullRequest> {
        pull.belongs_to(self)?;
        let number = pull.pull()?;
        let details = self.kind().pull_details(self, account, number).await?;
        self.checkout_details(pull.clone(), details)
    }

    /// The PR `url`, as the forge read it in `pull`, with where its head is.
    fn checkout_details(&self, url: ItemRef, pull: Current) -> Result<PullRequest> {
        let number = url.pull()?;
        let pr = pull.item.pr.context("not a PR")?;
        ensure!(
            !pr.head.as_str().is_empty() && !pr.base.as_str().is_empty(),
            "PR #{number} has no head or base branch"
        );
        // A deleted fork leaves no head repository.
        let fork = !pull
            .head_repository
            .is_some_and(|repository| repository.eq_ignore_ascii_case(&self.path));
        Ok(PullRequest {
            number,
            title: pull.item.title,
            url,
            labels: pull.item.labels,
            head: pr.head,
            base: pr.base,
            head_commit: pull.head_sha,
            fork,
            head_ref: self.kind().pull_head_ref(number),
        })
    }
}

pub(crate) const MERGED_HINT: &str = "; use `shoal done` to finish without a PR watch";

#[cfg(test)]
mod tests;
