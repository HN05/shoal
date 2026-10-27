//! Resolve issue context before workspace creation and agent launch.
use anyhow::{Context as _, Result, bail, ensure};

use crate::{
    cli::{client, context::Context, ui},
    forge::{ForgeRepo, IssueInput, items::link::ItemKind},
    git,
    model::Repository,
    repository,
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
    if let Some(root) = current_root().await? {
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

/// The registered repository whose checkout the current directory is in.
pub(super) async fn current_checkout(repos: &[Repository]) -> Result<Option<&Repository>> {
    let root = current_root().await?;
    Ok(repos.iter().find(|repo| Some(&repo.path) == root.as_ref()))
}

/// The root of the worktree the current directory is in.
async fn current_root() -> Result<Option<std::path::PathBuf>> {
    let cwd = std::env::current_dir()?;
    match git::worktree_root(&cwd).await {
        Ok(root) => Ok(Some(std::fs::canonicalize(root.trim_end())?)),
        Err(_) => Ok(None),
    }
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
        if forge.is_remote(&remote) {
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

/// The registered repository of a PR or issue, from `--repo`, its URL, or the
/// current checkout, offering to register one that is missing; `retry` names
/// the spelling that selects one when several share the URL's remote.
pub(super) async fn item_repository(
    ctx: &Context,
    kind: ItemKind,
    input: Option<&str>,
    repository: Option<String>,
    retry: &str,
) -> Result<Repository> {
    let mut repos = client::repositories(&ctx.paths).await?;
    let repository = match repository {
        Some(repository) => {
            let selector = ui::repository_selector(repository)?;
            match super::repositories::offer_unregistered(ctx, &selector).await? {
                Some(repo) => {
                    let id = repo.id.clone();
                    repos.push(repo);
                    id.into()
                }
                None => selector,
            }
        }
        None if let Some(input) =
            input.filter(|input| IssueInput::parse(input) == IssueInput::Url) =>
        {
            let forge = match kind {
                ItemKind::Pr => ForgeRepo::from_pull_url(input)?,
                ItemKind::Issue => ForgeRepo::from_issue_url(input)?,
            };
            registered_remote(ctx, &mut repos, forge, retry)
                .await?
                .into()
        }
        None => current_repository(ctx, repos.clone(), REPO_OR_URL)
            .await?
            .into(),
    };
    Ok(crate::repository::select(&repos, &repository)
        .await?
        .clone())
}

pub(in crate::cli) struct Issue {
    pub(in crate::cli) number: u64,
    pub(in crate::cli) title: String,
    pub(super) url: String,
    pub(super) details: String,
}

impl Issue {
    /// `<number>-<words>`: up to three title words, or `issue-<number>` when
    /// the title has none, so a bare number is never mistaken for a revision.
    pub fn branch_name(&self) -> String {
        let words = title_words(&self.title);
        if words.is_empty() {
            return format!("issue-{}", self.number);
        }
        let mut name = self.number.to_string();
        for (index, word) in words.iter().enumerate() {
            let room = BRANCH_NAME_LEN.saturating_sub(name.len() + 1);
            if word.len() > room {
                if index == 0 {
                    name.push('-');
                    name.push_str(&word[..room]);
                }
                break;
            }
            name.push('-');
            name.push_str(word);
        }
        name
    }

    /// `<repo>#<number>`: short, and unique across repositories sharing a Herdr workspace.
    pub fn tab_label(&self, repo: &Repository) -> String {
        format!("{}#{}", crate::repository::name(repo), self.number)
    }

    /// The task an issue workspace's agent starts with; the body is the
    /// issue's description when the workspace was created.
    pub fn prompt(&self) -> String {
        let mut prompt = format!(
            "Work on issue #{}: {}\n{}\n",
            self.number, self.title, self.url
        );
        let details = self.details.trim();
        if !details.is_empty() {
            prompt.push_str(&format!("\nIssue description:\n{details}\n"));
        }
        prompt.push_str("\n`shoal view` shows the issue with its comments and later edits.");
        prompt
    }
}

/// Fits a zmx session name, which keeps the first 32 characters.
const BRANCH_NAME_LEN: usize = 32;
const BRANCH_NAME_WORDS: usize = 3;
/// Title words that add length without naming the issue.
const FILLER_WORDS: &[&str] = &[
    "a", "an", "and", "are", "as", "at", "be", "by", "for", "from", "in", "is", "it", "its", "of",
    "on", "or", "s", "that", "the", "this", "to", "when", "with",
];

/// The first lowercase ASCII words of `title` that name the issue.
fn title_words(title: &str) -> Vec<String> {
    title
        .split(|c: char| !c.is_ascii_alphanumeric())
        .filter(|word| !word.is_empty())
        .map(str::to_ascii_lowercase)
        .filter(|word| !FILLER_WORDS.contains(&word.as_str()))
        .take(BRANCH_NAME_WORDS)
        .collect()
}

/// The forge that hosts the repository's origin remote.
pub(super) async fn origin_forge(ctx: &Context, repo: &Repository) -> Result<ForgeRepo> {
    let remote = repository::remote_url_from_path(&repo.path)
        .await?
        .context("forge lookup needs an origin remote")?;
    ForgeRepo::resolve(&remote, &ctx.forge_hosts()?).await
}

/// The issue `input` names in the repository, read with the user's logins.
pub(in crate::cli) async fn load(ctx: &Context, repo: &Repository, input: &str) -> Result<Issue> {
    let forge = origin_forge(ctx, repo).await?;
    let (number, url) = forge.issue(input)?;
    let (title, details) = forge.issue_details(&ctx.forge_account()?, number).await?;
    Ok(Issue {
        number,
        title,
        url,
        details,
    })
}

/// The URL of the issue `input` names: a number in the repository, or a URL.
pub(in crate::cli) async fn url(ctx: &Context, repo: &Repository, input: &str) -> Result<String> {
    let forge = match crate::forge::IssueInput::parse(input) {
        crate::forge::IssueInput::Url => {
            ForgeRepo::resolve_issue_url(input, &ctx.forge_hosts()?).await?
        }
        _ => origin_forge(ctx, repo).await?,
    };
    Ok(forge.issue(input)?.1)
}

/// The URLs of the open issues the forge records as blocking `issue`, read
/// with the user's logins.
pub(in crate::cli) async fn open_dependencies(
    ctx: &Context,
    repo: &Repository,
    issue: &Issue,
) -> Result<Vec<String>> {
    let forge = origin_forge(ctx, repo).await?;
    let mut urls = Vec::new();
    let account = ctx.forge_account()?;
    for url in forge
        .open_issue_dependencies(&account, issue.number)
        .await?
    {
        urls.push(ForgeRepo::from_issue_url(&url)?.issue(&url)?.1);
    }
    Ok(urls)
}

#[cfg(test)]
mod tests;
