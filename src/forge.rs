//! Forge identity and read-only PR queries using the user's gh/fj login.
use anyhow::{Context, Result, ensure};

#[derive(Debug)]
pub(crate) struct ForgeRepo {
    pub host: String,
    pub path: String,
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
        let (authority, path, ssh) = if let Some((scheme, rest)) = remote.split_once("://") {
            ensure!(
                matches!(scheme, "https" | "http" | "ssh"),
                "issue lookup needs a GitHub or Forgejo remote"
            );
            let (authority, path) = rest
                .split_once('/')
                .context("remote is missing owner/repository")?;
            (authority, path, scheme == "ssh")
        } else {
            let (authority, path) = remote
                .split_once(':')
                .context("issue lookup needs a GitHub or Forgejo remote")?;
            (authority, path, true)
        };
        let host = authority.rsplit('@').next().unwrap_or(authority);
        let host = if ssh {
            host.split(':').next().unwrap_or(host)
        } else {
            host
        };
        let path = path.trim_end_matches('/');
        let path = path.strip_suffix(".git").unwrap_or(path);
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
        Ok(Self {
            host: host.to_ascii_lowercase(),
            path: path.into(),
            web_scheme: if remote.starts_with("http://") {
                "http"
            } else {
                "https"
            },
        })
    }

    /// The repository an issue URL belongs to.
    pub fn from_issue_url(url: &str) -> Result<Self> {
        let url = url.split(['?', '#']).next().unwrap().trim_end_matches('/');
        let (repo, _) = url
            .rsplit_once("/issues/")
            .context("expected an issue URL ending in /issues/<number>")?;
        Self::parse(repo)
    }

    pub fn issue(&self, input: &str) -> Result<(u64, String)> {
        let (number, url) = if input.starts_with("https://") || input.starts_with("http://") {
            let input = input
                .split(['?', '#'])
                .next()
                .unwrap()
                .trim_end_matches('/');
            let (repo, number) = input
                .rsplit_once("/issues/")
                .context("expected an issue URL ending in /issues/<number>")?;
            ensure!(
                Self::parse(repo)? == *self,
                "issue URL belongs to a different repository"
            );
            (number, input.to_owned())
        } else {
            (
                input,
                format!(
                    "{}://{}/{}/issues/{input}",
                    self.web_scheme, self.host, self.path
                ),
            )
        };
        ensure!(
            !number.is_empty() && number.bytes().all(|c| c.is_ascii_digit()),
            "issue must be a positive number or an issue URL"
        );
        let number = number.parse::<u64>().context("issue number is too large")?;
        ensure!(number > 0, "issue number must be positive");
        Ok((number, url))
    }
}

impl ForgeRepo {
    pub fn pull(&self, input: &str) -> Result<(u64, String)> {
        let marker = if self.host == "github.com" {
            "/pull/"
        } else {
            "/pulls/"
        };
        let (number, url) = if input.starts_with("https://") || input.starts_with("http://") {
            let input = input
                .split(['?', '#'])
                .next()
                .unwrap()
                .trim_end_matches('/');
            let (repo, number) = input.rsplit_once(marker).context("invalid PR URL")?;
            ensure!(
                Self::parse(repo)? == *self,
                "PR belongs to a different repository"
            );
            (number, input.to_owned())
        } else {
            (
                input,
                format!(
                    "{}://{}/{}{marker}{input}",
                    self.web_scheme, self.host, self.path
                ),
            )
        };
        ensure!(
            !number.is_empty() && number.bytes().all(|b| b.is_ascii_digit()),
            "invalid PR number"
        );
        let number = number.parse::<u64>()?;
        ensure!(number > 0, "invalid PR number");
        Ok((number, url))
    }

    /// Fail closed on changed CLI output. Only commits actually listed in the
    /// merged PR can authorize cleanup, including squash/rebase merges.
    pub async fn merged_commits(
        &self,
        path: &std::path::Path,
        number: u64,
        branch: &str,
    ) -> Result<Option<Vec<String>>> {
        let number = number.to_string();
        if self.host == "github.com" {
            let output = query(
                path,
                "gh",
                &[
                    "pr",
                    "view",
                    &number,
                    "--repo",
                    &format!("{}/{}", self.host, self.path),
                    "--json",
                    "number,state,headRefName,commits",
                ],
                MERGED_HINT,
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
                "--style", "minimal", "pr", "view", &number, "--host", &self.host,
            ];
            let output = query(path, "fj", &args, MERGED_HINT).await?;
            if !fj_merged(&output, &number, branch)? {
                return Ok(None);
            }
            let mut args = args.to_vec();
            args.push("commits");
            let output = query(path, "fj", &args, MERGED_HINT).await?;
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
        let url = url.split(['?', '#']).next().unwrap().trim_end_matches('/');
        let (repo, _) = url
            .rsplit_once("/pull/")
            .or_else(|| url.rsplit_once("/pulls/"))
            .context("expected a PR URL ending in /pull/<number> or /pulls/<number>")?;
        Self::parse(repo)
    }

    /// Only PRs whose head branch lives in this repository can be checked out.
    pub async fn pull_request(&self, path: &std::path::Path, input: &str) -> Result<PullRequest> {
        let (number, url) = self.pull(input)?;
        let id = number.to_string();
        let (title, head, base) = if self.host == "github.com" {
            let repo = format!("{}/{}", self.host, self.path);
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
            let pull: Pull = serde_json::from_str(&query(path, "gh", &args, "").await?)
                .context("invalid gh PR response")?;
            ensure!(pull.number == number, "gh returned a different PR");
            ensure!(
                !pull.is_cross_repository,
                "PR #{number} comes from a fork; only branches in this repository can be opened"
            );
            (pull.title, pull.head_ref_name, pull.base_ref_name)
        } else {
            let args = [
                "--style", "minimal", "pr", "view", &id, "--host", &self.host,
            ];
            fj_pull(&query(path, "fj", &args, "").await?, &id)?
        };
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

/// Title, head and base from fj's minimal `pr view`.
fn fj_pull(text: &str, number: &str) -> Result<(String, String, String)> {
    let text: String = text
        .chars()
        .filter(|c| !matches!(c, '\u{2066}'..='\u{2069}'))
        .collect();
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
    Ok((title.to_owned(), head.to_owned(), base.to_owned()))
}

fn fj_merged(text: &str, number: &str, branch: &str) -> Result<bool> {
    let text: String = text
        .chars()
        .filter(|c| !matches!(c, '\u{2066}'..='\u{2069}'))
        .collect();
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

const MERGED_HINT: &str = "; otherwise confirm the merge yourself and run `shoal pr merged`";

async fn query(path: &std::path::Path, tool: &str, args: &[&str], hint: &str) -> Result<String> {
    let mut command = tokio::process::Command::new(tool);
    command.current_dir(path).args(args).env("NO_COLOR", "1");
    tokio::time::timeout(
        std::time::Duration::from_secs(20),
        crate::subprocess::output(command),
    )
    .await
    .context("PR lookup timed out")?
    .with_context(|| format!("PR lookup requires {tool} and its existing login{hint}"))
}

#[cfg(test)]
mod tests {
    use super::*;
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
        assert!(fj_pull("Title #56\nBy user\n", "56").is_err());
        let repo = ForgeRepo::from_pull_url("https://github.com/team/repo/pull/56/files").unwrap();
        assert_eq!(repo, ForgeRepo::parse("git@github.com:team/repo").unwrap());
        assert!(ForgeRepo::from_pull_url("https://github.com/team/repo/issues/56").is_err());
    }

    #[test]
    fn pr_numbers_resolve_against_the_forge_remote() {
        for (remote, url) in [
            (
                "git@github.com:team/repo.git",
                "https://github.com/team/repo/pull/56",
            ),
            (
                "ssh://git@forge.example:2222/team/repo.git",
                "https://forge.example/team/repo/pulls/56",
            ),
            (
                "http://forge.example:3000/team/repo.git",
                "http://forge.example:3000/team/repo/pulls/56",
            ),
        ] {
            let repo = ForgeRepo::parse(remote).unwrap();
            assert_eq!(repo.pull("56").unwrap(), (56, url.into()));
            assert_eq!(repo.pull(url).unwrap(), (56, url.into()));
            assert_eq!(
                repo.pull(&format!("{url}/?tab=files#diff")).unwrap(),
                (56, url.into())
            );
            for input in [
                "",
                "0",
                "-1",
                "+56",
                "#56",
                "56/",
                "56?x",
                "56#x",
                "abc",
                "18446744073709551616",
            ] {
                assert!(repo.pull(input).is_err(), "{input}");
            }
        }
    }

    #[test]
    fn forge_link_scheme_does_not_change_repository_identity() {
        let repo = ForgeRepo::parse("http://forge.example/team/repo.git").unwrap();
        assert_eq!(
            repo,
            ForgeRepo::parse("git@forge.example:team/repo.git").unwrap()
        );
        assert_eq!(
            repo.issue("56").unwrap().1,
            "http://forge.example/team/repo/issues/56"
        );
        assert_eq!(
            repo.pull("https://forge.example/team/repo/pulls/56")
                .unwrap()
                .1,
            "https://forge.example/team/repo/pulls/56"
        );
    }
}
