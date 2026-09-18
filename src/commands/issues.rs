//! Forge issue lookup belongs to the CLI; the daemon only creates workspaces.
use std::{ffi::OsString, process::Stdio, time::Duration};

use anyhow::{Context as _, Result, bail, ensure};
use serde::Deserialize;
use tokio::{process::Command, time::timeout};

use crate::{
    cli::Agent, client, config::Config, context::Context, forge::ForgeRepo, model::Repository,
    repository, ui, validate::MAX_NAME_LEN,
};

/// `shoal issue <url>`: the URL names the repository, the issue names the
/// workspace, and `--agent` or the configured default agent works on it.
pub(super) async fn run(
    ctx: &Context,
    url: String,
    agent: Option<Agent>,
    base: Option<String>,
    args: Vec<OsString>,
) -> Result<i32> {
    let repos = client::repositories(&ctx.paths).await?;
    let repo = repository_for(&repos, &url).await?;
    let agent = match agent.or(Config::load(&ctx.paths)?.default_agent) {
        Some(agent) => agent,
        None if ctx.interactive() => ui::pick(
            ctx,
            "Agent> ",
            Agent::possible_values()
                .into_iter()
                .map(|value| (value.clone(), value))
                .collect(),
        )?
        .parse()
        .map_err(|()| anyhow::anyhow!("unknown agent"))?,
        None => bail!("no agent selected; pass --agent or set default_agent in the global config"),
    };
    super::workspaces::add(
        ctx,
        Some(repo.id.clone()),
        (None, None),
        base,
        Some(url),
        Some(agent),
        args,
    )
    .await
}

/// The registered repository whose origin the issue URL belongs to.
async fn repository_for<'a>(repos: &'a [Repository], url: &str) -> Result<&'a Repository> {
    let forge = ForgeRepo::from_issue_url(url)?;
    let mut matches = Vec::new();
    for repo in repos {
        let Some(remote) = repository::remote_url(&repo.source).await? else {
            continue;
        };
        if ForgeRepo::parse(&remote).is_ok_and(|remote| remote == forge) {
            matches.push(repo);
        }
    }
    match matches.as_slice() {
        [repo] => Ok(repo),
        [] => bail!(
            "no registered repository has the remote {}/{}; run `shoal repo add <path-or-url>`",
            forge.host,
            forge.path
        ),
        _ => bail!(
            "several registered repositories share the remote {}/{}; use `shoal add <repository> --issue <url>`",
            forge.host,
            forge.path
        ),
    }
}

pub(super) struct Issue {
    number: u64,
    title: String,
    url: String,
    details: String,
}

impl Issue {
    pub fn branch_name(&self) -> String {
        let mut name = format!("issue-{}", self.number);
        let words = self.title.split(|c: char| !c.is_ascii_alphanumeric());
        for word in words.filter(|word| !word.is_empty()) {
            if name.len() + 1 >= MAX_NAME_LEN {
                break;
            }
            name.push('-');
            name.extend(
                word.chars()
                    .take(MAX_NAME_LEN - name.len())
                    .map(|c| c.to_ascii_lowercase()),
            );
        }
        name
    }

    pub fn prompt(&self) -> String {
        format!(
            "Work on issue #{}: {}\n{}\n\nIssue details:\n{}",
            self.number, self.title, self.url, self.details
        )
    }
}

pub(super) async fn load(repo: &Repository, input: &str) -> Result<Issue> {
    let remote =
        repository::remote_url(repo.path.to_str().context("repository path is not UTF-8")?)
            .await?
            .context("issue lookup needs an origin remote")?;
    let forge = ForgeRepo::parse(&remote)?;
    let (number, url) = forge.issue(input)?;
    let github = forge.host == "github.com";
    let tool = if github { "gh" } else { "fj" };
    let mut command = Command::new(tool);
    command.current_dir(&repo.path);
    if github {
        command.args([
            "issue",
            "view",
            &number.to_string(),
            "--repo",
            &format!("{}/{}", forge.host, forge.path),
            "--json",
            "number,title,body",
        ]);
    } else {
        command.args([
            "--style",
            "minimal",
            "issue",
            "view",
            &number.to_string(),
            "--host",
            &forge.host,
            "--remote",
            "origin",
        ]);
    }
    let output = timeout(
        Duration::from_secs(30),
        command.stdin(Stdio::null()).kill_on_drop(true).output(),
    )
    .await
    .context("issue lookup timed out")?
    .with_context(|| {
        format!("run {tool}; install it and run `{tool} auth login` before using --issue")
    })?;
    ensure!(
        output.status.success(),
        "{tool} issue lookup failed; check `{tool} auth login` and repository access: {}",
        String::from_utf8_lossy(&output.stderr)
            .chars()
            .take(2048)
            .collect::<String>()
    );
    let text = String::from_utf8(output.stdout).context("issue output is not UTF-8")?;
    let (title, details) = if github {
        #[derive(Deserialize)]
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
        forgejo_details(&text, number)?
    };
    ensure!(!title.trim().is_empty(), "issue title is empty");
    Ok(Issue {
        number,
        title,
        url,
        details,
    })
}

// fj currently has no JSON mode. Minimal output starts with `<title> #<id>`
// (some versions append a quote), with bidi isolates even when stdout is piped.
fn forgejo_details(text: &str, number: u64) -> Result<(String, String)> {
    let text: String = text
        .chars()
        .filter(|c| !matches!(c, '\u{2066}'..='\u{2069}'))
        .collect();
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn forge_urls_match_transports_and_reject_other_repositories() {
        let repo = ForgeRepo::parse("git@github.com:team/repo.git").unwrap();
        assert_eq!(
            repo,
            ForgeRepo::parse("ssh://git@github.com:2222/team/repo").unwrap()
        );
        assert_eq!(repo.issue("42").unwrap().0, 42);
        assert_eq!(
            repo.issue("https://github.com/team/repo/issues/42#comment")
                .unwrap()
                .0,
            42
        );
        for input in [
            "0",
            "-1",
            "--help",
            "https://other.test/team/repo/issues/42",
            "https://github.com/team/other/issues/42",
            "https://github.com/team/repo/pull/42",
        ] {
            assert!(repo.issue(input).is_err(), "{input}");
        }
        assert!(ForgeRepo::parse("/tmp/repo").is_err());
    }

    #[tokio::test]
    async fn issue_urls_select_exactly_one_registered_repository() {
        let repo = |id: &str, source: &str| Repository {
            id: id.into(),
            path: format!("/nonexistent/{id}").into(),
            source: source.into(),
            last_used: 0,
            name: None,
            workspaces_dir: None,
        };
        let repos = [
            repo("a", "https://github.com/team/repo.git"),
            repo("b", "git@forge.example:team/repo.git"),
            repo("c", "ssh://git@forge.example:2222/team/repo"),
        ];
        let url = "https://github.com/team/repo/issues/7#issuecomment-1";
        assert_eq!(repository_for(&repos, url).await.unwrap().id, "a");
        let error = repository_for(&repos, "https://forge.example/team/repo/issues/7")
            .await
            .unwrap_err()
            .to_string();
        assert!(error.contains("several"), "{error}");
        let error = repository_for(&repos, "https://github.com/team/other/issues/7")
            .await
            .unwrap_err()
            .to_string();
        assert!(error.contains("shoal repo add"), "{error}");
        assert!(
            repository_for(&repos, "https://github.com/team/repo/pull/7")
                .await
                .is_err()
        );
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
    fn issue_names_are_portable_bounded_and_stable() {
        let mut issue = Issue {
            number: 34,
            title: "Fix `API`: $(touch /tmp/no); 日本語".into(),
            url: "url".into(),
            details: "body".into(),
        };
        assert_eq!(issue.branch_name(), "issue-34-fix-api-touch-tmp-no");
        issue.title = "x".repeat(200);
        assert_eq!(issue.branch_name().len(), MAX_NAME_LEN);
        issue.title = "日本語".into();
        assert_eq!(issue.branch_name(), "issue-34");
        assert!(issue.prompt().contains("url\n\nIssue details:\nbody"));
    }
}
