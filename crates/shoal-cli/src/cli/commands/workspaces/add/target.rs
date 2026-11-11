//! The repository an addition goes to.
use anyhow::Result;

use crate::{
    cli::{
        commands::{issues, repositories},
        context::Context,
    },
    forge::IssueInput,
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

pub(super) async fn resolve_add_target(
    ctx: &Context,
    repository: Option<crate::repository::Selector>,
    issue: Option<&str>,
) -> Result<AddTarget> {
    // Load on first use so explicit targets keep their original failure order.
    let mut repositories = None;
    // A chat opens workspaces only in its own repository, which is
    // registered, so registering another is never offered there.
    let offer = !crate::env::is_scoped() && crate::env::in_chat().is_none();
    let selector = match repository {
        Some(selector) if !offer => selector,
        Some(selector) => repositories::offer_unregistered(ctx, &selector)
            .await?
            .map_or(selector, |repo| repo.id.into()),
        None => {
            let repos = repositories.insert(crate::client::repositories::list(&ctx.paths).await?);
            match issue {
                Some(url) if IssueInput::parse(url) == IssueInput::Url => {
                    issues::repository_for(ctx, repos, url, offer).await?.into()
                }
                Some(_) => issues::current_repository(ctx, repos.clone(), issues::REPO_OR_URL)
                    .await?
                    .into(),
                None => issues::current_repository(ctx, repos.clone(), repositories::REPOSITORY)
                    .await?
                    .into(),
            }
        }
    };
    Ok(AddTarget {
        selector,
        repositories: tokio::sync::OnceCell::new_with(repositories),
    })
}
