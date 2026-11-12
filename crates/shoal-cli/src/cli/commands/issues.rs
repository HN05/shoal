//! Resolve issue context before workspace creation and agent launch.
use anyhow::{Context as _, Result, bail, ensure};

use crate::client::repository_ids::RepositoryId;
use crate::client::space_names::SpaceSelector;
use crate::{
    cli::{client, context::Context, ui},
    forge::{
        ForgeRepo,
        items::{
            link::{ItemKind, LinkKind, Selection},
            reference::{ItemInput, ItemRef},
        },
    },
    git,
    model::{PresentSpace, Repository},
    protocol::{ItemContext, operations::LaunchIssue},
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
) -> Result<RepositoryId> {
    if let Some(id) = current_repository_id(ctx, &repos).await? {
        return Ok(id);
    }
    ensure!(
        ctx.interactive(),
        "no current registered repository; {hint}"
    );
    let picked = ui::pick(ctx, "Repository> ", ui::repository_choices(repos).await)?;
    Ok(picked.into())
}

/// The ID of the registered repository of the current checkout or
/// workspace, without asking.
pub(super) async fn current_repository_id(
    ctx: &Context,
    repos: &[Repository],
) -> Result<Option<RepositoryId>> {
    let Some(root) = current_root().await? else {
        return Ok(None);
    };
    if let Some(repo) = repos.iter().find(|repo| repo.path == root) {
        return Ok(Some(repo.id.clone()));
    }
    Ok(crate::client::workspaces::list(&ctx.paths)
        .await?
        .into_iter()
        .find(|workspace| workspace.path == root)
        .map(|workspace| workspace.repository_id))
}

/// The registered repository whose checkout the current directory is in.
pub(super) async fn current_checkout(repos: &[Repository]) -> Result<Option<&Repository>> {
    let root = current_root().await?;
    Ok(repos.iter().find(|repo| Some(&repo.path) == root.as_ref()))
}

/// The root of the worktree the current directory is in, if it still exists.
async fn current_root() -> Result<Option<std::path::PathBuf>> {
    let Some(cwd) = ui::current_directory()? else {
        return Ok(None);
    };
    match git::worktree_root(&cwd).await {
        Ok(root) => Ok(Some(std::fs::canonicalize(root.trim_end())?)),
        Err(_) => Ok(None),
    }
}

/// The registered repository whose origin the issue `issue` belongs to,
/// which the user may register when none has it, if `offer`.
pub(super) async fn repository_for(
    ctx: &Context,
    repos: &mut Vec<Repository>,
    issue: &ItemRef,
    offer: bool,
) -> Result<RepositoryId> {
    let (forge, retry) = (
        issue.repository().clone(),
        "shoal start <repository> --issue <url>",
    );
    if !offer {
        return Ok(repository_with_remote(repos, forge, retry)
            .await?
            .id
            .clone());
    }
    registered_remote(ctx, repos, forge, retry).await
}

/// The only registered repository whose origin `item` belongs to, or none
/// when no registered repository has that origin.
pub(super) async fn registered_for_item<'a>(
    repos: &'a [Repository],
    item: &ItemRef,
) -> Result<Option<&'a Repository>> {
    let forge = item.repository().clone();
    match repository_with_remote(repos, forge, "--repo <ID>").await {
        Ok(repo) => Ok(Some(repo)),
        Err(error) if error.is::<UnregisteredRemote>() => Ok(None),
        Err(error) => Err(error),
    }
}

/// The ID of the repository [`repository_with_remote`] selects. When none has
/// the remote, the user may register its URL, which joins `repos`.
pub(super) async fn registered_remote(
    ctx: &Context,
    repos: &mut Vec<Repository>,
    forge: ForgeRepo,
    retry: &str,
) -> Result<RepositoryId> {
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
    input: Option<&ItemInput>,
    repository: Option<String>,
    retry: &str,
) -> Result<Repository> {
    let mut repos = crate::client::repositories::list(&ctx.paths).await?;
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
        None if let Some(ItemInput::Url(item)) = input => {
            item.of_kind(kind.into())?;
            let forge = item.repository().clone();
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
    pub(super) url: ItemRef,
    pub(super) details: String,
    pub(in crate::cli) labels: Vec<String>,
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

    /// The issue's number and title, which name a place for its workspace.
    pub fn place_issue(&self) -> (u64, &str) {
        (self.number, &self.title)
    }

    /// The issue as an agent's subject, worded the same in every mode; the
    /// body is the issue's description when the agent starts.
    pub fn subject(&self) -> crate::config::templates::Subject {
        let mut text = format!("Issue #{}: {}\n{}\n", self.number, self.title, self.url);
        let details = self.details.trim();
        if !details.is_empty() {
            text.push_str(&format!("\nIssue description:\n{details}\n"));
        }
        text.push_str("\n`shoal view` shows the issue with its comments and later edits.");
        crate::config::templates::Subject {
            text,
            issue: Some(crate::config::templates::IssueFields {
                number: self.number,
                title: self.title.clone(),
                url: self.url.to_string(),
            }),
        }
    }

    /// The issue as a queued agent's launch names it.
    pub fn launch_issue(&self) -> LaunchIssue {
        LaunchIssue {
            url: self.url.clone(),
            title: self.title.clone(),
            labels: self.labels.clone(),
        }
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

/// Where a command reads from the forge of `repo`: the workspace or chat
/// this runs in, whose scope reads only there, when it is of `repo`, else
/// the repository's registered checkout.
pub(in crate::cli) async fn read_context(ctx: &Context, repo: &Repository) -> Result<ItemContext> {
    let own = if crate::env::workspace_scoped() {
        Some(ui::select_workspace(ctx, None, ui::Fallback::OwnWorkspace).await?)
    } else {
        crate::env::chat().map(SpaceSelector::Id)
    };
    if let Some(space) = own
        && let Some(space) = crate::client::workspaces::present_space(&ctx.paths, &space).await?
        && space.repository_id() == Some(&repo.id)
    {
        return Ok(match space {
            PresentSpace::Workspace(workspace) => {
                ItemContext::Workspace(SpaceSelector::id(&workspace.id))
            }
            PresentSpace::Chat(chat) => ItemContext::Chat(SpaceSelector::id(&chat.id)),
        });
    }
    Ok(ItemContext::Checkout(repo.id.clone()))
}

/// The open issue `input` names in the repository of `context`, by number
/// or URL.
pub(in crate::cli) async fn load(
    ctx: &Context,
    context: ItemContext,
    input: impl Into<ItemInput>,
) -> Result<Issue> {
    let issue = client::issue_details(&ctx.paths, context, input.into()).await?;
    Ok(Issue::from(issue))
}

/// [`load`] of the issue a workspace already has, open or closed.
pub(in crate::cli) async fn load_linked(
    ctx: &Context,
    context: ItemContext,
    input: impl Into<ItemInput>,
) -> Result<Issue> {
    let issue = client::linked_issue_details(&ctx.paths, context, input.into()).await?;
    Ok(Issue::from(issue))
}

impl From<crate::forge::items::issue::IssueDetails> for Issue {
    fn from(issue: crate::forge::items::issue::IssueDetails) -> Self {
        Self {
            number: issue.number.get(),
            title: issue.title,
            url: issue.url,
            details: issue.details,
            labels: issue.labels,
        }
    }
}

/// The issue `input` names: a number in the repository, or a URL.
pub(in crate::cli) async fn issue(
    ctx: &Context,
    repo: &Repository,
    input: &ItemInput,
) -> Result<ItemRef> {
    let selection = Selection {
        kind: Some(LinkKind::Issue),
        input: Some(input.clone()),
    };
    let context = read_context(ctx, repo).await?;
    let selected = client::select_items(&ctx.paths, context, selection, true).await?;
    let issue = selected.items.into_iter().next();
    Ok(issue.context("no issue selected")?.url)
}

#[cfg(test)]
mod tests;
