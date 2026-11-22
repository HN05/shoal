//! Resolve issue context before workspace creation and agent launch.
use anyhow::{Result, ensure};

use crate::{
    cli::{client, context::Context, ui},
    forge::{
        ForgeRepo,
        items::{
            link::ItemKind,
            reference::{ItemInput, ItemRef},
        },
    },
    model::Repository,
    protocol::ItemContext,
    repository,
    scoping::Scoping,
};
use shoal_ids::repository_ids::RepositoryId;
use shoal_protocol::space_names::SpaceSelector;

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
    let workspaces = crate::client::spaces::list(&ctx.paths)
        .await?
        .into_workspaces();
    Ok(repository::at_root(&root, repos, &workspaces).cloned())
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
    repository::worktree_root(&cwd).await
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
        return Ok(repository::with_remote(repos, forge, retry)
            .await?
            .id
            .clone());
    }
    registered_remote(ctx, repos, forge, retry).await
}

/// The ID of the repository [`repository_with_remote`] selects. When none has
/// the remote, the user may register its URL, which joins `repos`.
pub(super) async fn registered_remote(
    ctx: &Context,
    repos: &mut Vec<Repository>,
    forge: ForgeRepo,
    retry: &str,
) -> Result<RepositoryId> {
    let error = match repository::with_remote(repos, forge, retry).await {
        Ok(repo) => return Ok(repo.id.clone()),
        Err(error) => error,
    };
    let Some(url) = error
        .downcast_ref::<repository::UnregisteredRemote>()
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
}

impl Issue {
    /// The branch of the issue, as [`crate::launches::workspace::issue_branch`]
    /// names it.
    pub fn branch_name(&self) -> String {
        crate::launches::workspace::issue_branch(self.number, &self.title)
    }
}

/// Where a command reads from the forge of `repo`: the workspace or chat
/// this runs in, whose scope reads only there, when it is of `repo`, else
/// the repository's registered checkout.
pub(in crate::cli) async fn read_context(ctx: &Context, repo: &Repository) -> Result<ItemContext> {
    let own = match Scoping::of_workspace() {
        Scoping::Scoped => Some(ui::select_workspace(ctx, None, ui::Fallback::OwnSpace).await?),
        Scoping::Unscoped => crate::env::own_space().map(SpaceSelector::Id),
    };
    if let Some(space) = own
        && let Some(space) = crate::client::spaces::present_space(&ctx.paths, &space).await?
        && space.repository_id.as_ref() == Some(&repo.id)
    {
        return Ok(ItemContext::space(SpaceSelector::id(&space.id)));
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

impl From<crate::forge::items::issue::IssueDetails> for Issue {
    fn from(issue: crate::forge::items::issue::IssueDetails) -> Self {
        Self {
            number: issue.number.get(),
            title: issue.title,
            url: issue.url,
        }
    }
}

#[cfg(test)]
mod tests;
