//! Where a repository's local config lives, and what it holds.
use std::path::PathBuf;

use anyhow::{Context as _, Result};

use super::selection::chosen;
use crate::cli::context::Context;
use crate::client::output::repositories::LocalConfig;

pub(super) async fn show(ctx: &Context, repository: Option<String>) -> Result<i32> {
    let repository = chosen(ctx, repository).await?;
    let path = local_config_path(ctx, repository).await?;
    let toml =
        crate::fsutil::read_optional(&path).with_context(|| format!("read {}", path.display()))?;
    let config = LocalConfig { path, toml };
    ctx.show(&config, render)?;
    Ok(0)
}

/// The repository's local config file, whose directory the daemon reserves
/// when the repository has none yet.
pub(in crate::cli::commands) async fn local_config_path(
    ctx: &Context,
    repository: crate::repository::Selector,
) -> Result<PathBuf> {
    crate::client::repositories::local_config_path(&ctx.paths, repository).await
}

fn render(config: &LocalConfig) {
    outln!("{}", config.path.display());
    match &config.toml {
        Some(text) => out!("{text}"),
        None => eprintln!("No local config; create the file to add one"),
    }
}
