//! Repository selectors and caller-relative paths.
use std::path::PathBuf;

use anyhow::Result;

use crate::{
    cli::{client, commands::issues, context::Context, ui},
    repository,
};

/// The named repository, else the current checkout's or workspace's, else
/// one chosen from the registered repositories.
pub(in crate::cli::commands) async fn chosen(
    ctx: &Context,
    repository: Option<String>,
) -> Result<repository::Selector> {
    match repository {
        Some(repository) => ui::repository_selector(repository),
        None => {
            let repos = client::repositories(&ctx.paths).await?;
            Ok(issues::current_repository(ctx, repos, REPOSITORY)
                .await?
                .into())
        }
    }
}

/// How a refusal names an omitted `<repository>` argument.
pub(in crate::cli::commands) const REPOSITORY: &str = "pass <repository>";

/// Expand `~` and resolve relative paths against the caller's directory.
pub(in crate::cli::commands) fn absolute(ctx: &Context, path: PathBuf) -> Result<PathBuf> {
    let path = crate::fsutil::expand_home(&path, &ctx.paths.home);
    Ok(if path.is_absolute() {
        path
    } else {
        std::env::current_dir()?.join(path)
    })
}
