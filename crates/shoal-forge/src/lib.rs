//! Issues, PRs, review threads and CI on the forge a Git remote names,
//! through each forge's own CLI login or REST API. Callers choose the
//! [`account::Account`] a request acts as; this crate never decides who may act.
pub mod account;
pub mod backends;
pub mod ci;
pub mod conventions;
pub mod items;
pub mod repo;
pub mod review;
mod state;
pub mod transport;
pub mod updates;

use anyhow::{Context, Result, ensure};

use backends::{Backend, ForgeKind};
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

#[derive(Debug)]
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
        let remote = RemoteUrl::parse(remote).context("issue lookup needs a forge remote")?;
        Self::from_remote(&remote)
    }

    fn from_remote(remote: &RemoteUrl<'_>) -> Result<Self> {
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
        let host = host.to_ascii_lowercase();
        let kind = ForgeKind::detect(&host);
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
            web_scheme: if remote.transport == Transport::Http {
                "http"
            } else {
                "https"
            },
        })
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
        ItemRoute::discover(url, &[ItemRoute::GitLabIssue, ItemRoute::Issue])
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
        ItemRoute::discover(
            url,
            &[
                ItemRoute::Pull,
                ItemRoute::Pulls,
                ItemRoute::MergeRequest,
                ItemRoute::PullRequests,
            ],
        )
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

pub(crate) const MERGED_HINT: &str = "; use `shoal done` to finish without a PR watch";

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn issue_input_classification_defers_validation_until_lookup() {
        let repo = ForgeRepo::parse("https://forge.example/team/repo").unwrap();
        for input in ["0", "18446744073709551616"] {
            assert_eq!(IssueInput::parse(input), IssueInput::Number);
            assert!(repo.issue(input).is_err());
        }
        for input in ["", "-1", "+1", "#1", "1/", "１２", "not-an-issue"] {
            assert_eq!(IssueInput::parse(input), IssueInput::Invalid);
            assert!(repo.issue(input).is_err());
        }
        for input in [
            "https://forge.example/team/repo/issues/1?x#comment",
            "http://forge.example/team/repo/issues/1/",
        ] {
            assert_eq!(IssueInput::parse(input), IssueInput::Url);
            assert_eq!(repo.issue(input).unwrap().0, 1);
        }
        for input in [
            "https://forge.example/team/repo/pulls/1",
            "https://forge.example/team/repo/issues/0",
            "https://forge.example/team/other/issues/1",
        ] {
            assert_eq!(IssueInput::parse(input), IssueInput::Url);
            assert!(repo.issue(input).is_err());
        }
        assert_eq!(IssueInput::parse("001"), IssueInput::Number);
        assert_eq!(repo.issue("001").unwrap().0, 1);
    }

    #[test]
    fn backend_is_selected_from_the_normalized_remote_host() {
        for (remote, kind) in [
            ("git@GitHub.COM:team/repo.git", ForgeKind::GitHub),
            ("ssh://git@github.com:2222/team/repo", ForgeKind::GitHub),
            ("https://forge.example/team/repo.git", ForgeKind::Forgejo),
            ("http://forge.example:3000/team/repo", ForgeKind::Forgejo),
            ("git@gitlab.com:group/sub/project.git", ForgeKind::GitLab),
            ("git@bitbucket.org:workspace/repo.git", ForgeKind::Bitbucket),
        ] {
            assert_eq!(ForgeRepo::parse(remote).unwrap().kind, kind);
        }
    }

    #[test]
    fn bitbucket_items_use_pull_request_routes() {
        let repo = ForgeRepo::parse("git@bitbucket.org:workspace/repo.git").unwrap();
        let base = "https://bitbucket.org/workspace/repo";
        assert_eq!(repo.issue("4").unwrap(), (4, format!("{base}/issues/4")));
        let url = format!("{base}/pull-requests/5");
        assert_eq!(repo.pull("5").unwrap(), (5, url.clone()));
        assert_eq!(
            ForgeRepo::from_pull_url(&format!("{url}/diff")).unwrap(),
            repo
        );
        assert!(repo.pull(&format!("{base}/pulls/5")).is_err());
        assert_eq!(
            repo.repo_endpoint("pullrequests/5"),
            "repositories/workspace/repo/pullrequests/5"
        );
    }

    #[test]
    fn only_gitlab_paths_nest_groups() {
        let repo = ForgeRepo::parse("https://gitlab.com/group/sub/project.git").unwrap();
        assert_eq!(repo.path, "group/sub/project");
        assert_eq!(
            repo.repo_endpoint("issues/3"),
            "projects/group%2Fsub%2Fproject/issues/3"
        );
        for remote in [
            "https://github.com/group/sub/repo",
            "https://forge.example/group/sub/repo",
            "https://gitlab.com/project",
            "https://gitlab.com/group/-/project",
            "https://gitlab.com/group//project",
        ] {
            assert!(ForgeRepo::parse(remote).is_err(), "{remote}");
        }
    }

    #[test]
    fn gitlab_items_use_their_own_routes() {
        let repo = ForgeRepo::parse("git@gitlab.com:group/sub/project.git").unwrap();
        let base = "https://gitlab.com/group/sub/project";
        assert_eq!(repo.issue("4").unwrap(), (4, format!("{base}/-/issues/4")));
        assert_eq!(
            repo.pull(&format!("{base}/-/merge_requests/5/diffs"))
                .unwrap_err()
                .to_string(),
            "invalid PR number"
        );
        assert_eq!(
            repo.pull(&format!("{base}/-/merge_requests/5#note_1"))
                .unwrap(),
            (5, format!("{base}/-/merge_requests/5"))
        );
        for (url, kind) in [
            (format!("{base}/-/issues/4"), "issue"),
            (format!("{base}/-/merge_requests/5"), "pr"),
        ] {
            let discovered = if kind == "issue" {
                ForgeRepo::from_issue_url(&url)
            } else {
                ForgeRepo::from_pull_url(&url)
            };
            assert_eq!(discovered.unwrap(), repo, "{url}");
        }
        assert!(repo.issue(&format!("{base}/issues/4")).is_err());
        assert!(repo.pull(&format!("{base}/pulls/5")).is_err());
        assert!(
            repo.pull(&format!("{base}/-/merge_requests/5/../6"))
                .is_err()
        );
    }

    #[test]
    fn pr_identity_is_strict() {
        let repo = ForgeRepo::parse("git@example.com:team/repo.git").unwrap();
        assert_eq!(
            repo.pull("https://example.com/team/repo/pulls/56").unwrap(),
            (56, "https://example.com/team/repo/pulls/56".into())
        );
        for url in [
            "https://example.com/other/repo/pulls/56",
            "https://other.com/team/repo/pulls/56",
            "https://example.com/team/repo/pulls/0",
        ] {
            assert!(repo.pull(url).is_err());
        }
    }

    #[test]
    fn pull_urls_name_their_repository() {
        let repo = ForgeRepo::from_pull_url("https://github.com/team/repo/pull/56/files").unwrap();
        assert_eq!(repo, ForgeRepo::parse("git@github.com:team/repo").unwrap());
        assert!(ForgeRepo::from_pull_url("https://github.com/team/repo/issues/56").is_err());
    }

    #[test]
    fn item_locators_share_validation_and_preserve_links() {
        for (remote, base, pull_route) in [
            (
                "git@github.com:team/Repo.git",
                "https://github.com/team/Repo",
                "pull",
            ),
            (
                "ssh://git@forge.example:2222/team/Repo.git",
                "https://forge.example/team/Repo",
                "pulls",
            ),
            (
                "http://forge.example:3000/team/Repo.git",
                "http://forge.example:3000/team/Repo",
                "pulls",
            ),
        ] {
            let repo = ForgeRepo::parse(remote).unwrap();
            for (route, lookup) in [
                (
                    "issues",
                    ForgeRepo::issue as fn(&ForgeRepo, &str) -> Result<(u64, String)>,
                ),
                (pull_route, ForgeRepo::pull),
            ] {
                for (digits, number) in
                    [("56", 56), ("0056", 56), ("18446744073709551615", u64::MAX)]
                {
                    let url = format!("{base}/{route}/{digits}");
                    assert_eq!(lookup(&repo, digits).unwrap(), (number, url.clone()));
                    for suffix in [
                        "",
                        "/",
                        "///",
                        "?tab=files",
                        "#comment",
                        "/?tab=files#comment",
                        "#comment?query",
                    ] {
                        let input = format!("{url}{suffix}");
                        assert_eq!(
                            lookup(&repo, &input).unwrap(),
                            (number, url.clone()),
                            "{input}"
                        );
                    }
                    // Supplied links retain their scheme, host case, .git and leading zeroes.
                    let spelled = format!(
                        "https://{}/team/Repo.git/{route}/{digits}",
                        repo.host.to_uppercase()
                    );
                    assert_eq!(lookup(&repo, &spelled).unwrap(), (number, spelled));
                }
                for invalid in [
                    "",
                    "0",
                    "-1",
                    "+56",
                    "#56",
                    "56/",
                    "56?x",
                    "56#x",
                    " 56",
                    "56 ",
                    "１２",
                    "HEAD",
                    "main~1",
                    "team/Repo#56",
                    "18446744073709551616",
                ] {
                    assert!(lookup(&repo, invalid).is_err(), "{route}: {invalid}");
                    let url = format!("{base}/{route}/{invalid}");
                    // URL suffixes are stripped, but bare-number suffixes are invalid.
                    if !["56/", "56?x", "56#x"].contains(&invalid) {
                        assert!(lookup(&repo, &url).is_err(), "{url}");
                    }
                }
                for url in [
                    format!("https://other.example/team/Repo/{route}/56"),
                    format!("{base}-other/{route}/56"),
                    format!("{base}/{route}/56/files"),
                    format!("{base}/{route}/%35%36"),
                    format!("ssh://git@{}/{}/{route}/56", repo.host, repo.path),
                    format!("git@{}:{}/{route}/56", repo.host, repo.path),
                    format!("//{}/{}/{route}/56", repo.host, repo.path),
                    format!("HTTPS://{}/{}/{route}/56", repo.host, repo.path),
                ] {
                    assert!(lookup(&repo, &url).is_err(), "{url}");
                }
                for wrong_route in ["issues", "pull", "pulls", "commits"] {
                    if wrong_route != route {
                        let url = format!("{base}/{wrong_route}/56");
                        assert!(lookup(&repo, &url).is_err(), "{url}");
                    }
                }
            }
        }
    }

    #[test]
    fn item_locator_errors_identify_the_item_kind() {
        let repo = ForgeRepo::parse("https://github.com/team/repo").unwrap();
        for (input, issue, pull) in [
            ("0", "issue number must be positive", "invalid PR number"),
            (
                "18446744073709551616",
                "issue number is too large",
                "PR number is too large",
            ),
            (
                "HEAD",
                "issue must be a positive number or an issue URL",
                "invalid PR number",
            ),
            (
                "https://github.com/team/repo",
                "expected an issue URL ending in /issues/<number>",
                "invalid PR URL",
            ),
        ] {
            assert_eq!(repo.issue(input).unwrap_err().to_string(), issue);
            assert_eq!(repo.pull(input).unwrap_err().to_string(), pull);
        }
        for (input, expected) in [
            (
                "https://github.com/other/repo/issues/1",
                "issue URL belongs to a different repository",
            ),
            (
                "https://github.com/other/repo/pull/1",
                "PR belongs to a different repository",
            ),
        ] {
            let error = if input.contains("/issues/") {
                repo.issue(input)
            } else {
                repo.pull(input)
            };
            assert_eq!(error.unwrap_err().to_string(), expected);
        }
    }

    #[test]
    fn item_repository_discovery_defers_number_validation() {
        for (route, discover, lookup) in [
            (
                "issues",
                ForgeRepo::from_issue_url as fn(&str) -> Result<ForgeRepo>,
                ForgeRepo::issue as fn(&ForgeRepo, &str) -> Result<(u64, String)>,
            ),
            ("pull", ForgeRepo::from_pull_url, ForgeRepo::pull),
        ] {
            for tail in [
                "56/?query#fragment",
                "0",
                "18446744073709551616",
                "56/files",
            ] {
                let url = format!("http://GitHub.com/team/Repo/{route}/{tail}");
                let repo = discover(&url).unwrap();
                assert_eq!(
                    repo,
                    ForgeRepo::parse("git@github.com:team/Repo.git").unwrap()
                );
                assert_eq!(repo.web_scheme, "http");
                assert_eq!(lookup(&repo, &url).is_ok(), tail == "56/?query#fragment");
            }
        }
        let url = "https://forge.example/team/Repo/pulls/56/?query#fragment";
        assert_eq!(
            ForgeRepo::from_pull_url(url).unwrap(),
            ForgeRepo::parse("git@forge.example:team/Repo.git").unwrap()
        );
    }
}
