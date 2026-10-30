//! Issues, PRs, review threads and CI on the forge a Git remote names,
//! through each forge's own CLI login or REST API. Callers choose the
//! [`account::Account`] a request acts as; this crate never decides who may act.
pub mod account;
pub mod backends;
pub mod capability;
pub mod ci;
pub mod conventions;
pub mod hosts;
pub mod items;
pub mod raw;
pub mod repo;
pub mod review;
mod state;
#[cfg(test)]
mod test_support;
pub mod transport;
pub mod updates;

use anyhow::{Context, Result, ensure};

use backends::{Backend, ForgeKind};
use hosts::{HostType, HostTypes};
use repo::{
    locator::ItemRoute,
    remote_url::{RemoteUrl, Transport},
};

/// Classify before resolving a repository; validate identity and number at lookup.
/// In particular, zero and overflowing numbers still use number-based targeting.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum IssueInput {
    Number,
    Url,
    Invalid,
}

impl IssueInput {
    pub fn parse(input: &str) -> Self {
        if input.starts_with("https://") || input.starts_with("http://") {
            Self::Url
        } else if !input.is_empty() && input.bytes().all(|c| c.is_ascii_digit()) {
            Self::Number
        } else {
            Self::Invalid
        }
    }
}

#[derive(Clone, Debug)]
pub struct ForgeRepo {
    pub host: String,
    pub path: String,
    kind: ForgeKind,
    web_scheme: &'static str,
}

impl PartialEq for ForgeRepo {
    fn eq(&self, other: &Self) -> bool {
        // Transport is not part of repository identity.
        self.host == other.host && self.path == other.path
    }
}

impl ForgeRepo {
    pub fn parse(remote: &str) -> Result<Self> {
        Self::parse_linked(remote, None)
    }

    /// The repository `remote` names, where a link that names it uses the
    /// routes of the forge `linked`.
    pub(crate) fn parse_linked(remote: &str, linked: Option<ForgeKind>) -> Result<Self> {
        let remote = RemoteUrl::parse(remote).context("issue lookup needs a forge remote")?;
        let (host, path, web_scheme) = Self::location(&remote)?;
        let kind = ForgeKind::detect(&host, linked);
        Self::on(kind, host, path, web_scheme)
    }

    /// The repository `remote` names, on the forge `hosts` says its host
    /// serves, which this process then remembers when the host answered it.
    pub async fn resolve(remote: &str, hosts: &impl HostTypes) -> Result<Self> {
        let remote = RemoteUrl::parse(remote).context("issue lookup needs a forge remote")?;
        let (host, path, web_scheme) = Self::location(&remote)?;
        let resolved = hosts.forge_type(&host, web_scheme).await?;
        if let HostType::Answered(kind) = resolved {
            hosts::learn(&host, kind);
        }
        let kind = resolved.kind();
        Self::on(kind, host, path, web_scheme)
    }

    /// The web host, repository path and web scheme of `remote`.
    fn location<'a>(remote: &RemoteUrl<'a>) -> Result<(String, &'a str, &'static str)> {
        ensure!(
            matches!(
                remote.transport,
                Transport::Http | Transport::Https | Transport::Ssh | Transport::Scp
            ),
            "issue lookup needs an HTTP or SSH forge remote"
        );
        let path = remote.path.context("remote is missing owner/repository")?;
        let host = remote.host.unwrap_or_default();
        // SSH ports affect registration identity, but not the forge's web host.
        let host = if matches!(remote.transport, Transport::Ssh | Transport::Scp) {
            host.split(':').next().unwrap_or(host)
        } else {
            host
        };
        ensure!(
            !host.is_empty()
                && !host.starts_with('-')
                && host
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | ':')),
            "issue lookup needs a remote with host/owner/repository"
        );
        let web_scheme = if remote.transport == Transport::Http {
            "http"
        } else {
            "https"
        };
        Ok((host.to_ascii_lowercase(), path, web_scheme))
    }

    /// The repository at `path` on `host`, which serves `kind`.
    fn on(kind: ForgeKind, host: String, path: &str, web_scheme: &'static str) -> Result<Self> {
        let segments = path.split('/').count();
        ensure!(
            (segments == 2 || kind.nests_groups() && segments > 2)
                && path.split('/').all(|part| !part.is_empty()
                    && !matches!(part, "." | ".." | "-")
                    && part
                        .chars()
                        .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_'))),
            "issue lookup needs a remote with host/owner/repository"
        );
        Ok(Self {
            kind,
            host,
            path: path.into(),
            web_scheme,
        })
    }

    /// Whether `remote` names this repository, read as a remote of the same
    /// forge, so a nested GitLab project matches before its host resolves.
    pub fn is_remote(&self, remote: &str) -> bool {
        Self::parse_linked(remote, Some(self.kind)).is_ok_and(|remote| remote == *self)
    }

    /// The repository `input` names: a remote or web URL, on any forge, or
    /// an `owner/repository` path on this repository's host.
    pub async fn resolve_named(&self, input: &str, hosts: &impl HostTypes) -> Result<Self> {
        if input.contains(':') {
            return Self::resolve(input, hosts).await;
        }
        let path = repo::remote_url::repository_path(input);
        Self::on(self.kind, self.host.clone(), path, self.web_scheme)
            .with_context(|| format!("{input} names no repository"))
    }

    /// `host` as a repository without a path, for asking it as `kind`.
    pub(crate) fn host(host: &str, web_scheme: &'static str, kind: ForgeKind) -> Self {
        Self {
            kind,
            host: host.into(),
            path: String::new(),
            web_scheme,
        }
    }

    pub async fn issue_details(
        &self,
        account: &account::Account,
        number: u64,
    ) -> Result<(String, String)> {
        let (title, details) = self.kind.issue_details(self, account, number).await?;
        ensure!(!title.trim().is_empty(), "issue title is empty");
        Ok((title, details))
    }

    /// The repository an issue URL belongs to.
    pub fn from_issue_url(url: &str) -> Result<Self> {
        let (repository, route) = ItemRoute::discover(url, ISSUE_ROUTES)?;
        Self::parse_linked(repository, route.forge())
    }

    /// The repository an issue URL belongs to, on the forge `hosts` says
    /// its host serves.
    pub async fn resolve_issue_url(url: &str, hosts: &impl HostTypes) -> Result<Self> {
        Self::resolve(ItemRoute::discover(url, ISSUE_ROUTES)?.0, hosts).await
    }

    /// The user, organization or workspace the repository belongs to.
    pub(crate) fn owner(&self) -> &str {
        self.path.split('/').next().unwrap_or_default()
    }

    pub fn repository_url(&self) -> String {
        format!("{}://{}/{}", self.web_scheme, self.host, self.path)
    }

    pub fn issue(&self, input: &str) -> Result<(u64, String)> {
        self.kind.issue_route().resolve(self, input)
    }
}

impl ForgeRepo {
    pub fn pull(&self, input: &str) -> Result<(u64, String)> {
        self.kind.pull_route().resolve(self, input)
    }

    /// Fail closed on changed output. Only commits actually listed in the
    /// merged PR can authorize cleanup, including squash/rebase merges.
    pub async fn merged_commits(
        &self,
        account: &account::Account,
        number: u64,
        branch: &str,
    ) -> Result<Option<Vec<String>>> {
        self.kind
            .merged_commits(self, account, number, branch)
            .await
    }
}

/// A PR's branches, for checking it out locally.
#[derive(Debug, PartialEq)]
pub struct PullRequest {
    pub number: u64,
    pub title: String,
    pub url: String,
    pub head: String,
    pub base: String,
}

impl ForgeRepo {
    /// The repository a PR URL belongs to.
    pub fn from_pull_url(url: &str) -> Result<Self> {
        let (repository, route) = ItemRoute::discover(url, PULL_ROUTES)?;
        Self::parse_linked(repository, route.forge())
    }

    /// The repository a PR URL belongs to, on the forge `hosts` says its
    /// host serves.
    pub async fn resolve_pull_url(url: &str, hosts: &impl HostTypes) -> Result<Self> {
        Self::resolve(ItemRoute::discover(url, PULL_ROUTES)?.0, hosts).await
    }

    /// Only PRs whose head branch lives in this repository can be checked out.
    pub async fn pull_request(
        &self,
        account: &account::Account,
        input: &str,
    ) -> Result<PullRequest> {
        let (number, url) = self.pull(input)?;
        let (title, head, base) = self.kind.pull_details(self, account, number).await?;
        ensure!(
            !head.is_empty() && !base.is_empty(),
            "PR #{number} has no head or base branch"
        );
        Ok(PullRequest {
            number,
            title,
            url,
            head,
            base,
        })
    }
}

const ISSUE_ROUTES: &[ItemRoute] = &[ItemRoute::GitLabIssue, ItemRoute::Issue];

const PULL_ROUTES: &[ItemRoute] = &[
    ItemRoute::Pull,
    ItemRoute::Pulls,
    ItemRoute::MergeRequest,
    ItemRoute::PullRequests,
];

pub(crate) const MERGED_HINT: &str = "; use `shoal done` to finish without a PR watch";

#[cfg(test)]
mod tests;
