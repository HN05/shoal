//! Forge issue lookup belongs to the CLI; the daemon only creates workspaces.
use anyhow::{Context as _, Result, bail, ensure};

use crate::{
    client,
    context::Context,
    forge::{ForgeRepo, repository},
    git,
    model::Repository,
    ui,
    validate::MAX_NAME_LEN,
};

pub(super) async fn repository_for_number(ctx: &Context, repos: Vec<Repository>) -> Result<String> {
    let cwd = std::env::current_dir()?;
    if let Ok(root) = git::run(&cwd, &["rev-parse", "--show-toplevel"]).await {
        let root = std::fs::canonicalize(root.trim_end())?;
        if let Some(repo) = repos.iter().find(|repo| repo.path == root) {
            return Ok(repo.id.clone());
        }
        if let Some(workspace) = client::workspaces(&ctx.paths)
            .await?
            .iter()
            .find(|workspace| workspace.path == root)
        {
            return Ok(workspace.repository_id.clone());
        }
    }
    ensure!(
        ctx.interactive(),
        "no current registered repository; pass --repo <repository> or a forge URL"
    );
    ui::pick(ctx, "Repository> ", ui::repository_choices(repos).await?)
}

/// The registered repository whose origin the issue URL belongs to.
pub(super) async fn repository_for<'a>(
    repos: &'a [Repository],
    url: &str,
) -> Result<&'a Repository> {
    repository_with_remote(
        repos,
        ForgeRepo::from_issue_url(url)?,
        "shoal add <repository> --issue <url>",
    )
    .await
}

/// The only registered repository whose origin is `forge`; `retry` names the
/// spelling that selects one explicitly.
pub(super) async fn repository_with_remote<'a>(
    repos: &'a [Repository],
    forge: ForgeRepo,
    retry: &str,
) -> Result<&'a Repository> {
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
            "several registered repositories share the remote {}/{}; use `{retry}`",
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

    pub fn prompt(&self, template: Option<&str>) -> String {
        crate::config::templates::render(
            template.unwrap_or(crate::config::templates::ISSUE_DEFAULT),
            &[
                ("{number}", &self.number.to_string()),
                ("{title}", &self.title),
                ("{url}", &self.url),
                ("{body}", &self.details),
            ],
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
    let (title, details) = forge.issue_details(&repo.path, number).await?;
    Ok(Issue {
        number,
        title,
        url,
        details,
    })
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
        assert!(issue.prompt(None).contains("url\n\nIssue details:\nbody"));
    }
}
