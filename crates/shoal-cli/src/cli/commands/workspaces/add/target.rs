//! The repository an addition goes to.
use anyhow::Result;
use shoal_protocol::space_names::SpaceSelector;

use super::reads::DaemonReads;
use crate::{
    cli::{
        commands::{issues, repositories},
        context::Context,
    },
    forge::items::reference::ItemInput,
    launches::LaunchReads,
    model::Repository,
};

pub(super) struct AddTarget {
    pub(super) selector: crate::repository::Selector,
    pub(super) repositories: tokio::sync::OnceCell<Vec<Repository>>,
}

impl AddTarget {
    pub(super) async fn repository(&self, ctx: &Context) -> Result<&Repository> {
        let repos = self
            .repositories
            .get_or_try_init(|| crate::client::repositories::list(&ctx.paths))
            .await?;
        crate::repository::select(repos, &self.selector).await
    }
}

/// The repository a start goes in: the one `repository` names, offering to
/// register it, else the one the `issue` URL belongs to, else that of the
/// `parent` space it goes below, which a child shares, else the current or
/// a picked one.
pub(super) async fn resolve_add_target(
    ctx: &Context,
    repository: Option<crate::repository::Selector>,
    issue: Option<&ItemInput>,
    parent: Option<&SpaceSelector>,
) -> Result<AddTarget> {
    // Load on first use so explicit targets keep their original failure order.
    let mut repositories = None;
    // A chat opens workspaces only in its own repository, which is
    // registered, so registering another is never offered there.
    let offer = !crate::env::is_scoped() && crate::env::own_chat().is_none();
    let selector = match repository {
        Some(selector) if !offer => selector,
        Some(selector) => repositories::offer_unregistered(ctx, &selector)
            .await?
            .map_or(selector, |repo| repo.id.into()),
        None => {
            let repos = repositories.insert(crate::client::repositories::list(&ctx.paths).await?);
            unnamed_repository(ctx, repos, (issue, parent), offer).await?
        }
    };
    Ok(AddTarget {
        selector,
        repositories: tokio::sync::OnceCell::new_with(repositories),
    })
}

/// The repository of a start that names none: the one the `issue` URL
/// belongs to among `repos`, offering to register it where `offer`, else
/// that of the `parent` space it goes below, which a child shares, else the
/// current or a picked one.
async fn unnamed_repository(
    ctx: &Context,
    repos: &mut Vec<Repository>,
    (issue, parent): (Option<&ItemInput>, Option<&SpaceSelector>),
    offer: bool,
) -> Result<crate::repository::Selector> {
    if let Some(ItemInput::Url(issue)) = issue {
        return Ok(issues::repository_for(ctx, repos, issue, offer)
            .await?
            .into());
    }
    if let Some(parent) = parent_repository(ctx, parent).await? {
        return Ok(parent.into());
    }
    let hint = match issue {
        Some(_) => issues::REPO_OR_URL,
        None => repositories::REPOSITORY,
    };
    Ok(issues::current_repository(ctx, repos.clone(), hint)
        .await?
        .into())
}

/// The repository of the space `parent` names; none without a parent or
/// for a chat without a repository.
async fn parent_repository(
    ctx: &Context,
    parent: Option<&SpaceSelector>,
) -> Result<Option<shoal_ids::repository_ids::RepositoryId>> {
    match parent {
        Some(parent) => DaemonReads(ctx).space_repository(parent).await,
        None => Ok(None),
    }
}
