//! The repository an addition goes to.
use anyhow::Result;

use crate::{
    cli::{
        client,
        commands::{issues, repositories},
        context::Context,
        ui,
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
            .get_or_try_init(|| client::repositories(&ctx.paths))
            .await?;
        crate::repository::select(repos, &self.selector).await
    }
}

pub(super) async fn resolve_add_target(
    ctx: &Context,
    repository: Option<String>,
    issue: Option<&str>,
) -> Result<AddTarget> {
    // Load on first use so explicit targets keep their original failure order.
    let mut repositories = None;
    let selector = match repository {
        Some(repo) => {
            let selector = ui::repository_selector(repo)?;
            repositories::offer_unregistered(ctx, &selector)
                .await?
                .map_or(selector, |repo| repo.id.into())
        }
        None => {
            let repos = repositories.insert(client::repositories(&ctx.paths).await?);
            match issue {
                Some(url) if IssueInput::parse(url) == IssueInput::Url => {
                    issues::repository_for(ctx, repos, url).await?.into()
                }
                Some(_) => issues::current_repository(ctx, repos.clone(), issues::REPO_OR_URL)
                    .await?
                    .into(),
                None => {
                    ui::require_argument(ctx, "repository", "<repository>")?;
                    ui::pick(
                        ctx,
                        "Repository> ",
                        ui::repository_choices(repos.clone()).await?,
                    )?
                    .into()
                }
            }
        }
    };
    Ok(AddTarget {
        selector,
        repositories: tokio::sync::OnceCell::new_with(repositories),
    })
}
