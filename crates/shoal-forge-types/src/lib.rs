//! What Shoal reads of a forge without sending it a request: repositories,
//! issues, PRs and CI runs parsed from their URLs, and the data the
//! requests in `shoal-forge` return, which the daemon's protocol carries.
pub mod backends;
pub mod capability;
pub mod ci;
pub mod conventions;
pub mod download;
pub mod hosts;
pub mod items;
pub mod numbers;
pub mod raw;
pub mod repo;
pub mod review;
pub mod updates;

use anyhow::{Context, Result, ensure};
use serde::{Deserialize, Serialize};
use shoal_refs::{BranchName, CommitId};

use backends::{ForgeKind, Routes};
use items::reference::ItemRef;
use numbers::{IssueNumber, PullNumber};
use repo::remote_url::{RemoteUrl, Transport};

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
    pub fn parse_linked(remote: &str, linked: Option<ForgeKind>) -> Result<Self> {
        let remote = RemoteUrl::parse(remote).context("issue lookup needs a forge remote")?;
        let (host, path, web_scheme) = Self::location(&remote)?;
        let kind = ForgeKind::detect(&host, linked);
        Self::on(kind, host, path, web_scheme)
    }

    /// The web host, repository path and web scheme of `remote`.
    pub fn location<'a>(remote: &RemoteUrl<'a>) -> Result<(String, &'a str, &'static str)> {
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
    pub fn on(kind: ForgeKind, host: String, path: &str, web_scheme: &'static str) -> Result<Self> {
        ensure!(
            repository_path_valid(path, kind.nests_groups()),
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

    /// Refuse `input` when it names no repository on any forge, before
    /// [`resolve_named`](Self::resolve_named) asks a host which forge it is.
    pub fn check_named(input: &str) -> Result<()> {
        let path = if input.contains(':') {
            let remote =
                RemoteUrl::parse(input).with_context(|| format!("{input} names no repository"))?;
            Self::location(&remote)?.1
        } else {
            repo::remote_url::repository_path(input)
        };
        // A forge that nests groups takes the longest paths.
        ensure!(
            repository_path_valid(path, true),
            "{input} names no repository"
        );
        Ok(())
    }

    /// `host` as a repository without a path, for asking it as `kind`.
    pub fn host(host: &str, web_scheme: &'static str, kind: ForgeKind) -> Self {
        Self {
            kind,
            host: host.into(),
            path: String::new(),
            web_scheme,
        }
    }

    /// The user, organization or workspace the repository belongs to.
    pub fn owner(&self) -> &str {
        self.path.split('/').next().unwrap_or_default()
    }

    /// The forge the repository is on.
    pub fn kind(&self) -> ForgeKind {
        self.kind
    }

    /// The scheme of the forge's web URLs, `http` or `https`.
    pub fn web_scheme(&self) -> &'static str {
        self.web_scheme
    }

    pub fn repository_url(&self) -> String {
        format!("{}://{}/{}", self.web_scheme, self.host, self.path)
    }

    pub fn issue(&self, input: &str) -> Result<(IssueNumber, String)> {
        let (number, url) = self.kind.issue_route().resolve(self, input)?;
        Ok((IssueNumber::new(number), url))
    }

    pub fn pull(&self, input: &str) -> Result<(PullNumber, String)> {
        let (number, url) = self.kind.pull_route().resolve(self, input)?;
        Ok((PullNumber::new(number), url))
    }
}

/// A PR's branches, for checking it out locally.
#[derive(Debug, PartialEq, Serialize, Deserialize)]
pub struct PullRequest {
    pub number: PullNumber,
    pub title: String,
    pub url: ItemRef,
    /// Its head branch, in this repository or in `fork`.
    pub head: BranchName,
    pub base: BranchName,
    /// The commit its head branch points to, where the forge reports it.
    pub head_commit: Option<CommitId>,
    /// Whether its head branch is in another repository, or in one that was
    /// deleted, so this repository has no branch of it.
    pub fork: bool,
    /// The ref this repository serves its head under, where the forge has
    /// one, from which a fork's head is fetched.
    pub head_ref: Option<String>,
}

/// Whether `path` names a repository as `owner/name`, or below nested groups
/// where the forge `nests` them.
fn repository_path_valid(path: &str, nests: bool) -> bool {
    let segments = path.split('/').count();
    (segments == 2 || nests && segments > 2)
        && path.split('/').all(|part| {
            !part.is_empty()
                && !matches!(part, "." | ".." | "-")
                && part
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_'))
        })
}

#[cfg(test)]
mod tests;
