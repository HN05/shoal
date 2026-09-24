//! Forge identity and read-only issue/PR queries using the user's gh/fj login.
mod locator;
pub mod pr;
mod remote_url;
pub mod repository;

use crate::tools::Tool;
use anyhow::{Context, Result, ensure};

use locator::ItemRoute;
use remote_url::{RemoteUrl, Transport};

/// Classify before resolving a repository; validate identity and number at lookup.
/// In particular, zero and overflowing numbers still use number-based targeting.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum IssueInput {
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
pub(crate) struct ForgeRepo {
    pub host: String,
    pub path: String,
    kind: ForgeKind,
    web_scheme: &'static str,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ForgeKind {
    GitHub,
    Forgejo,
}

impl ForgeKind {
    fn from_host(host: &str) -> Self {
        if host == "github.com" {
            Self::GitHub
        } else {
            Self::Forgejo
        }
    }

    fn tool(self) -> &'static str {
        match self {
            Self::GitHub => Tool::GitHub.program(),
            Self::Forgejo => Tool::Forgejo.program(),
        }
    }

    fn pull_route(self) -> ItemRoute {
        match self {
            Self::GitHub => ItemRoute::Pull,
            Self::Forgejo => ItemRoute::Pulls,
        }
    }
}

impl PartialEq for ForgeRepo {
    fn eq(&self, other: &Self) -> bool {
        // Transport is not part of repository identity.
        self.host == other.host && self.path == other.path
    }
}

impl ForgeRepo {
    pub fn parse(remote: &str) -> Result<Self> {
        let remote =
            RemoteUrl::parse(remote).context("issue lookup needs a GitHub or Forgejo remote")?;
        Self::from_remote(&remote)
    }

    fn from_remote(remote: &RemoteUrl<'_>) -> Result<Self> {
        ensure!(
            matches!(
                remote.transport,
                Transport::Http | Transport::Https | Transport::Ssh | Transport::Scp
            ),
            "issue lookup needs a GitHub or Forgejo remote"
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
                    .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | ':'))
                && path.split('/').count() == 2
                && path.split('/').all(|part| !part.is_empty()
                    && part != "."
                    && part != ".."
                    && part
                        .chars()
                        .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_'))),
            "issue lookup needs a remote with host/owner/repository"
        );
        let host = host.to_ascii_lowercase();
        Ok(Self {
            kind: ForgeKind::from_host(&host),
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
        path: &std::path::Path,
        number: u64,
    ) -> Result<(String, String)> {
        self.kind.issue_details(self, path, number).await
    }

    /// The repository an issue URL belongs to.
    pub fn from_issue_url(url: &str) -> Result<Self> {
        ItemRoute::Issue.repository(url)
    }

    pub fn issue(&self, input: &str) -> Result<(u64, String)> {
        ItemRoute::Issue.resolve(self, input)
    }
}

impl ForgeRepo {
    pub fn pull(&self, input: &str) -> Result<(u64, String)> {
        self.kind.pull_route().resolve(self, input)
    }

    /// Fail closed on changed CLI output. Only commits actually listed in the
    /// merged PR can authorize cleanup, including squash/rebase merges.
    pub async fn merged_commits(
        &self,
        path: &std::path::Path,
        number: u64,
        branch: &str,
    ) -> Result<Option<Vec<String>>> {
        self.kind.merged_commits(self, path, number, branch).await
    }
}

impl ForgeKind {
    async fn merged_commits(
        self,
        repo: &ForgeRepo,
        path: &std::path::Path,
        number: u64,
        branch: &str,
    ) -> Result<Option<Vec<String>>> {
        let number = number.to_string();
        if self == Self::GitHub {
            let output = self
                .query(
                    path,
                    &[
                        "pr",
                        "view",
                        &number,
                        "--repo",
                        &format!("{}/{}", repo.host, repo.path),
                        "--json",
                        "number,state,headRefName,commits",
                    ],
                    Query::Pull(MERGED_HINT),
                )
                .await?;
            #[derive(serde::Deserialize)]
            struct Commit {
                oid: String,
            }
            #[derive(serde::Deserialize)]
            struct Pull {
                number: u64,
                state: String,
                #[serde(rename = "headRefName")]
                branch: String,
                commits: Vec<Commit>,
            }
            let pull: Pull = serde_json::from_str(&output)?;
            ensure!(
                pull.number.to_string() == number && pull.branch == branch,
                "PR does not match the workspace branch"
            );
            ensure!(
                matches!(pull.state.as_str(), "OPEN" | "CLOSED" | "MERGED"),
                "unknown PR state"
            );
            Ok((pull.state == "MERGED").then(|| pull.commits.into_iter().map(|c| c.oid).collect()))
        } else {
            let args = [
                "--style", "minimal", "pr", "view", &number, "--host", &repo.host,
            ];
            let output = self.query(path, &args, Query::Pull(MERGED_HINT)).await?;
            if !fj_merged(&output, &number, branch)? {
                return Ok(None);
            }
            let mut args = args.to_vec();
            args.push("commits");
            let output = self.query(path, &args, Query::Pull(MERGED_HINT)).await?;
            Ok(Some(
                output
                    .lines()
                    .filter_map(|line| {
                        let hash = line.strip_prefix("commit ")?.split_whitespace().next()?;
                        (hash.len() == 40 && hash.bytes().all(|b| b.is_ascii_hexdigit()))
                            .then(|| hash.to_owned())
                    })
                    .collect(),
            ))
        }
    }
}

impl ForgeKind {
    async fn issue_details(
        self,
        repo: &ForgeRepo,
        path: &std::path::Path,
        number: u64,
    ) -> Result<(String, String)> {
        let id = number.to_string();
        let (title, details) = if self == Self::GitHub {
            let repo = format!("{}/{}", repo.host, repo.path);
            let args = [
                "issue",
                "view",
                &id,
                "--repo",
                &repo,
                "--json",
                "number,title,body",
            ];
            let text = self.query(path, &args, Query::Issue).await?;
            #[derive(serde::Deserialize)]
            struct GitHubIssue {
                number: u64,
                title: String,
                body: Option<String>,
            }
            let issue: GitHubIssue =
                serde_json::from_str(&text).context("invalid gh issue response")?;
            ensure!(issue.number == number, "gh returned a different issue");
            (issue.title, issue.body.unwrap_or_default())
        } else {
            let args = [
                "--style", "minimal", "issue", "view", &id, "--host", &repo.host, "--remote",
                "origin",
            ];
            forgejo_details(&self.query(path, &args, Query::Issue).await?, number)?
        };
        ensure!(!title.trim().is_empty(), "issue title is empty");
        Ok((title, details))
    }
}

// fj currently has no JSON mode. Minimal output starts with `<title> #<id>`
// (some versions append a quote), with bidi isolates even when stdout is piped.
fn forgejo_details(text: &str, number: u64) -> Result<(String, String)> {
    let text = strip_bidi_isolates(text);
    let text = text.trim();
    let (header, details) = text
        .split_once('\n')
        .context("unrecognized fj issue output")?;
    let suffix = format!(" #{number}");
    let title = header
        .trim_end()
        .trim_end_matches('"')
        .strip_suffix(&suffix)
        .context("unrecognized fj issue title; expected title and issue number")?;
    Ok((title.to_owned(), details.trim().to_owned()))
}

/// A PR's branches, for checking it out locally.
#[derive(Debug, PartialEq)]
pub(crate) struct PullRequest {
    pub number: u64,
    pub title: String,
    pub url: String,
    pub head: String,
    pub base: String,
}

impl ForgeRepo {
    /// The repository a PR URL belongs to.
    pub fn from_pull_url(url: &str) -> Result<Self> {
        let item = ItemRoute::Pull
            .split_url(url)
            .or_else(|| ItemRoute::Pulls.split_url(url))
            .context("expected a PR URL ending in /pull/<number> or /pulls/<number>")?;
        Self::parse(item.repository)
    }

    /// Only PRs whose head branch lives in this repository can be checked out.
    pub async fn pull_request(&self, path: &std::path::Path, input: &str) -> Result<PullRequest> {
        let (number, url) = self.pull(input)?;
        let (title, head, base) = self.kind.pull_details(self, path, number).await?;
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

impl ForgeKind {
    async fn pull_details(
        self,
        repo: &ForgeRepo,
        path: &std::path::Path,
        number: u64,
    ) -> Result<(String, String, String)> {
        let id = number.to_string();
        if self == Self::GitHub {
            let repo = format!("{}/{}", repo.host, repo.path);
            let fields = "number,title,headRefName,baseRefName,isCrossRepository";
            let args = ["pr", "view", &id, "--repo", &repo, "--json", fields];
            #[derive(serde::Deserialize)]
            #[serde(rename_all = "camelCase")]
            struct Pull {
                number: u64,
                title: String,
                head_ref_name: String,
                base_ref_name: String,
                is_cross_repository: bool,
            }
            let pull: Pull = serde_json::from_str(&self.query(path, &args, Query::Pull("")).await?)
                .context("invalid gh PR response")?;
            ensure!(pull.number == number, "gh returned a different PR");
            ensure!(
                !pull.is_cross_repository,
                "PR #{number} comes from a fork; only branches in this repository can be opened"
            );
            Ok((pull.title, pull.head_ref_name, pull.base_ref_name))
        } else {
            let args = [
                "--style", "minimal", "pr", "view", &id, "--host", &repo.host,
            ];
            fj_pull(&self.query(path, &args, Query::Pull("")).await?, &id)
        }
    }
}

/// Title, head and base from fj's minimal `pr view`.
fn fj_pull(text: &str, number: &str) -> Result<(String, String, String)> {
    let text = strip_bidi_isolates(text);
    let mut lines = text.lines();
    let title = lines
        .next()
        .and_then(|s| s.trim_end().strip_suffix(&format!(" #{number}")))
        .context("unrecognized fj PR header")?;
    let (head, base) = lines
        .nth(1)
        .and_then(|s| {
            s.strip_prefix("From `")?
                .strip_suffix('`')?
                .split_once("` into `")
        })
        .context("unrecognized fj PR branches")?;
    // fj names a fork's head `<repository>:<branch>`; branch names cannot contain `:`.
    ensure!(
        !head.contains(':'),
        "PR #{number} comes from a fork; only branches in this repository can be opened"
    );
    Ok((title.to_owned(), head.to_owned(), base.to_owned()))
}

fn fj_merged(text: &str, number: &str, branch: &str) -> Result<bool> {
    let text = strip_bidi_isolates(text);
    let mut lines = text.lines();
    ensure!(
        lines
            .next()
            .is_some_and(|s| s.trim_end().ends_with(&format!(" #{number}"))),
        "unrecognized fj PR header"
    );
    let state = lines
        .next()
        .and_then(|s| s.split(" — ").nth(1))
        .context("unrecognized fj PR state")?;
    ensure!(
        matches!(state, "Open" | "Closed" | "Merged"),
        "unrecognized fj PR state"
    );
    ensure!(
        lines
            .next()
            .is_some_and(|s| s.starts_with(&format!("From `{branch}` into `")) && s.ends_with('`')),
        "PR does not match the workspace branch"
    );
    Ok(state == "Merged")
}

fn strip_bidi_isolates(text: &str) -> String {
    text.chars()
        .filter(|c| !matches!(c, '\u{2066}'..='\u{2069}'))
        .collect()
}

const MERGED_HINT: &str = "; otherwise confirm the merge yourself and run `shoal pr merged`";

#[derive(Clone, Copy)]
enum Query {
    Issue,
    Pull(&'static str),
}

impl ForgeKind {
    async fn query(self, path: &std::path::Path, args: &[&str], query: Query) -> Result<String> {
        let tool = self.tool();
        let mut command = tokio::process::Command::new(tool);
        command.current_dir(path).args(args);
        let seconds = match query {
            Query::Issue => 30,
            Query::Pull(_) => {
                command.env("NO_COLOR", "1");
                20
            }
        };
        let output = crate::subprocess::Run::new(command)
            .timeout(std::time::Duration::from_secs(seconds))
            .capture()
            .await;
        query.response(tool, output)
    }
}

impl Query {
    fn response(self, tool: &str, output: std::io::Result<std::process::Output>) -> Result<String> {
        match self {
            Self::Issue => {
                crate::subprocess::checked_output(tool, output).with_context(|| {
                    format!("{tool} issue lookup failed; install it and check `{tool} auth login` and repository access before using --issue")
                })
            }
            Self::Pull(hint) => crate::subprocess::checked_output(tool, output)
                .with_context(|| format!("PR lookup requires {tool} and its existing login{hint}")),
        }
    }
}

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
    fn query_errors_preserve_login_guidance_and_diagnostic_limits() {
        use std::{io, os::unix::process::ExitStatusExt, process::Output};

        for tool in ["gh", "fj"] {
            for (query, limit, hint) in [
                (
                    Query::Issue,
                    crate::subprocess::MAX_DIAGNOSTIC_CHARS,
                    "auth login",
                ),
                (
                    Query::Pull(MERGED_HINT),
                    crate::subprocess::MAX_DIAGNOSTIC_CHARS,
                    "shoal pr merged",
                ),
                (
                    Query::Pull(""),
                    crate::subprocess::MAX_DIAGNOSTIC_CHARS,
                    "existing login",
                ),
            ] {
                let missing = query
                    .response(tool, Err(io::Error::from(io::ErrorKind::NotFound)))
                    .unwrap_err();
                let missing = format!("{missing:#}");
                assert!(missing.contains(tool), "{missing}");
                assert!(missing.contains(hint), "{missing}");

                let failed = query
                    .response(
                        tool,
                        Ok(Output {
                            status: std::process::ExitStatus::from_raw(256),
                            stdout: b"ignored".to_vec(),
                            stderr: format!("{}END", "é".repeat(limit)).into_bytes(),
                        }),
                    )
                    .unwrap_err();
                let failed = format!("{failed:#}");
                assert!(failed.contains(tool), "{failed}");
                assert!(failed.contains(hint), "{failed}");
                assert!(failed.ends_with(&"é".repeat(limit)));
                assert!(!failed.contains("END"));

                let invalid = query
                    .response(
                        tool,
                        Ok(Output {
                            status: std::process::ExitStatus::from_raw(0),
                            stdout: vec![0xff],
                            stderr: Vec::new(),
                        }),
                    )
                    .unwrap_err();
                assert!(format!("{invalid:#}").contains("output is not UTF-8"));
            }
        }
    }

    #[test]
    fn fj_minimal_output_preserves_issue_details() {
        let text = "\u{2068}Add issue workspaces\u{2069} #\u{2068}34\u{2069}\"\nBy user — Open\n\n> Needs fj and gh\n\n0 comments\n";
        let (title, details) = forgejo_details(text, 34).unwrap();
        assert_eq!(title, "Add issue workspaces");
        assert!(details.contains("Needs fj and gh"));
        assert!(forgejo_details(text, 35).is_err());
        assert!(forgejo_details("changed output", 34).is_err());
    }

    #[test]
    fn backend_is_selected_from_the_normalized_remote_host() {
        for (remote, kind, tool) in [
            ("git@GitHub.COM:team/repo.git", ForgeKind::GitHub, "gh"),
            (
                "ssh://git@github.com:2222/team/repo",
                ForgeKind::GitHub,
                "gh",
            ),
            (
                "https://forge.example/team/repo.git",
                ForgeKind::Forgejo,
                "fj",
            ),
            (
                "http://forge.example:3000/team/repo",
                ForgeKind::Forgejo,
                "fj",
            ),
        ] {
            let repo = ForgeRepo::parse(remote).unwrap();
            assert_eq!(repo.kind, kind);
            assert_eq!(repo.kind.tool(), tool);
        }
    }

    #[test]
    fn pr_identity_and_state_are_strict() {
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
        let output = "Title #56\nBy user — Merged — +1 -0\nFrom `feature` into `main`\n\n> Merged";
        assert!(fj_merged(output, "56", "feature").unwrap());
        assert!(!fj_merged(&output.replacen("— Merged", "— Closed", 1), "56", "feature").unwrap());
        assert!(!fj_merged(&output.replacen("— Merged", "— Open", 1), "56", "feature").unwrap());
        assert!(fj_merged(output, "57", "feature").is_err());
        assert!(fj_merged(output, "56", "other").is_err());
        assert!(fj_merged("Merged", "56", "feature").is_err());
    }

    #[test]
    fn fj_pull_reads_title_and_branches() {
        let output = "\u{2068}Fix `x` #2\u{2069} #\u{2068}56\u{2069}\nBy user — Open — +1 -0\n\u{2068}From `\u{2068}feature/a\u{2069}` into `\u{2068}main\u{2069}`\u{2069}\n";
        assert_eq!(
            fj_pull(output, "56").unwrap(),
            ("Fix `x` #2".into(), "feature/a".into(), "main".into())
        );
        assert!(fj_pull(output, "57").is_err());
        let fork = output.replace(
            "From `\u{2068}feature/a",
            "From `\u{2068}someone/repo:feature/a",
        );
        assert!(
            fj_pull(&fork, "56")
                .unwrap_err()
                .to_string()
                .contains("fork")
        );
        assert!(fj_pull("Title #56\nBy user\n", "56").is_err());
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
