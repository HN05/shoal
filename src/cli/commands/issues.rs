//! Resolve issue context before workspace creation and agent launch.
use anyhow::{Context as _, Result, bail, ensure};

use crate::{
    cli::{client, context::Context, ui},
    forge::{ForgeRepo, repository},
    git,
    model::Repository,
    validate::MAX_NAME_LEN,
};

/// The explicit alternative for commands that accept `--repo` or a forge URL.
pub(super) const REPO_OR_URL: &str = "pass --repo <repository> or a forge URL";

/// The registered repository of the current checkout or workspace, otherwise
/// one chosen interactively; `hint` names the explicit alternative.
pub(super) async fn current_repository(
    ctx: &Context,
    repos: Vec<Repository>,
    hint: &str,
) -> Result<String> {
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
        "no current registered repository; {hint}"
    );
    ui::pick(ctx, "Repository> ", ui::repository_choices(repos).await?)
}

/// The registered repository whose origin the issue URL belongs to.
pub(super) async fn repository_for(
    ctx: &Context,
    repos: &mut Vec<Repository>,
    url: &str,
) -> Result<String> {
    registered_remote(
        ctx,
        repos,
        ForgeRepo::from_issue_url(url)?,
        "shoal add <repository> --issue <url>",
    )
    .await
}

/// The ID of the repository [`repository_with_remote`] selects. When none has
/// the remote, the user may register its URL, which joins `repos`.
pub(super) async fn registered_remote(
    ctx: &Context,
    repos: &mut Vec<Repository>,
    forge: ForgeRepo,
    retry: &str,
) -> Result<String> {
    let error = match repository_with_remote(repos, forge, retry).await {
        Ok(repo) => return Ok(repo.id.clone()),
        Err(error) => error,
    };
    let Some(url) = error
        .downcast_ref::<UnregisteredRemote>()
        .map(|remote| remote.0.repository_url())
    else {
        return Err(error);
    };
    let Some(repo) = super::repositories::offer_registration(ctx, &url).await? else {
        return Err(error);
    };
    let id = repo.id.clone();
    repos.push(repo);
    Ok(id)
}

#[derive(Debug)]
struct UnregisteredRemote(ForgeRepo);

impl std::fmt::Display for UnregisteredRemote {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "no registered repository has the remote {}/{}; register it with `shoal repo add {}` and retry",
            self.0.host,
            self.0.path,
            self.0.repository_url()
        )
    }
}

impl std::error::Error for UnregisteredRemote {}

/// The only registered repository whose origin is `forge`; `retry` names the
/// spelling that selects one explicitly.
async fn repository_with_remote<'a>(
    repos: &'a [Repository],
    forge: ForgeRepo,
    retry: &str,
) -> Result<&'a Repository> {
    let mut matches = Vec::new();
    for repo in repos {
        let Some(remote) = repository::remote_url_from_source(&repo.source).await? else {
            continue;
        };
        if ForgeRepo::parse(&remote).is_ok_and(|remote| remote == forge) {
            matches.push(repo);
        }
    }
    match matches.as_slice() {
        [repo] => Ok(repo),
        [] => Err(UnregisteredRemote(forge).into()),
        _ => bail!(
            "several registered repositories share the remote {}/{}; use `{retry}`",
            forge.host,
            forge.path
        ),
    }
}

pub(in crate::cli) struct Issue {
    pub(in crate::cli) number: u64,
    pub(in crate::cli) title: String,
    pub(super) url: String,
    pub(super) details: String,
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

    /// `<repo>#<number>`: short, and unique across repositories sharing a Herdr workspace.
    pub fn tab_label(&self, repo: &Repository) -> String {
        format!("{}#{}", crate::forge::repository::name(repo), self.number)
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

/// The forge that hosts the repository's origin remote.
pub(super) async fn origin_forge(repo: &Repository) -> Result<ForgeRepo> {
    let remote = repository::remote_url_from_path(&repo.path)
        .await?
        .context("forge lookup needs an origin remote")?;
    ForgeRepo::parse(&remote)
}

pub(super) async fn load(repo: &Repository, input: &str) -> Result<Issue> {
    let forge = origin_forge(repo).await?;
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

    async fn repository_for<'a>(repos: &'a [Repository], url: &str) -> Result<&'a Repository> {
        repository_with_remote(repos, ForgeRepo::from_issue_url(url)?, "retry").await
    }

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
        assert!(
            error.contains("shoal repo add https://github.com/team/other"),
            "{error}"
        );
        assert!(
            repository_for(&repos, "https://github.com/team/repo/pull/7")
                .await
                .is_err()
        );
    }

    #[tokio::test]
    async fn missing_repositories_suggest_the_repository_url() {
        for (url, repository) in [
            (
                "https://github.com/team/repo/issues/7#issuecomment-1",
                "https://github.com/team/repo",
            ),
            (
                "http://FORGE.example:3000/team/Repo.git/issues/7/?query#comment",
                "http://forge.example:3000/team/Repo",
            ),
            (
                "https://user:secret@forge.example/team/repo/issues/7",
                "https://forge.example/team/repo",
            ),
        ] {
            let error = repository_for(&[], url).await.unwrap_err().to_string();
            assert_eq!(
                error,
                format!(
                    "no registered repository has the remote {}; register it with `shoal repo add {repository}` and retry",
                    repository.split_once("://").unwrap().1,
                )
            );
        }
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
