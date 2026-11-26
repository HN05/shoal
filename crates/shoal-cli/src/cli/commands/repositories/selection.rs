use std::path::PathBuf;

use anyhow::Result;

use crate::{
    cli::{commands::issues, context::Context, ui},
    repository,
};

pub(in crate::cli::commands) async fn chosen(
    ctx: &Context,
    repository: Option<String>,
) -> Result<repository::Selector> {
    match repository {
        Some(repository) => ui::repository_selector(repository),
        None => {
            let repos = crate::client::repositories::list(&ctx.paths).await?;
            Ok(issues::current_repository(ctx, repos, REPOSITORY)
                .await?
                .into())
        }
    }
}

pub(in crate::cli::commands) const REPOSITORY: &str = "pass <repository>";

/// Expand `~` and resolve relative paths against the caller's directory.
pub(in crate::cli::commands) fn absolute(ctx: &Context, path: PathBuf) -> Result<PathBuf> {
    let path = crate::fsutil::home::expand_home(&path, &ctx.paths.home);
    Ok(if path.is_absolute() {
        path
    } else {
        std::env::current_dir()?.join(path)
    })
}
