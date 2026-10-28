//! Repository selectors and caller-relative paths.
use std::path::PathBuf;

use anyhow::Result;

use crate::{
    cli::{client, context::Context, ui},
    repository,
};

/// The named repository, else one chosen from the registered repositories.
pub(in crate::cli::commands) async fn chosen(
    ctx: &Context,
    repository: Option<String>,
) -> Result<repository::Selector> {
    match repository {
        Some(repository) => ui::repository_selector(repository),
        None => {
            let repos = client::repositories(&ctx.paths).await?;
            Ok(ui::pick(ctx, "Repository> ", ui::repository_choices(repos).await?)?.into())
        }
    }
}

/// Expand `~` and resolve relative paths against the caller's directory.
pub(in crate::cli::commands) fn absolute(ctx: &Context, path: PathBuf) -> Result<PathBuf> {
    let path = crate::fsutil::expand_home(&path, &ctx.paths.home);
    Ok(if path.is_absolute() {
        path
    } else {
        std::env::current_dir()?.join(path)
    })
}
