//! Forge identity and read-only PR queries using the user's gh/fj login.
use anyhow::{Context, Result, ensure};

#[derive(Debug, PartialEq)]
pub(crate) struct ForgeRepo {
    pub host: String,
    pub path: String,
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
                format!("https://{}/{}/issues/{input}", self.host, self.path),
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
                format!("https://{}/{}{marker}{input}", self.host, self.path),
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
            let output = query(path, "fj", &args).await?;
            if !fj_merged(&output, &number, branch)? {
                return Ok(None);
            }
            let mut args = args.to_vec();
            args.push("commits");
            let output = query(path, "fj", &args).await?;
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

async fn query(path: &std::path::Path, tool: &str, args: &[&str]) -> Result<String> {
    let mut command = tokio::process::Command::new(tool);
    command.current_dir(path).args(args).env("NO_COLOR", "1");
    tokio::time::timeout(std::time::Duration::from_secs(20), crate::subprocess::output(command))
        .await.context("PR lookup timed out")?
        .with_context(|| format!("PR lookup requires {tool} and its existing login; otherwise confirm the merge yourself and run `shoal pr merged`"))
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
                "https://forge.example:3000/team/repo/pulls/56",
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
}
