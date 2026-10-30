//! Where a repository's local config lives, and what it holds.
use std::path::PathBuf;

use anyhow::{Context as _, Result};
use serde::Serialize;

use super::selection::chosen;
use crate::{cli::client::request, cli::context::Context, protocol::RepositoryMethod};

#[derive(Serialize)]
struct LocalConfig {
    path: PathBuf,
    /// Unset while the file does not exist.
    toml: Option<String>,
}

pub(super) async fn show(ctx: &Context, repository: Option<String>) -> Result<i32> {
    let repository = chosen(ctx, repository).await?;
    let path =
        request::<PathBuf>(&ctx.paths, RepositoryMethod::LocalConfigPath { repository }).await?;
    let toml =
        crate::fsutil::read_optional(&path).with_context(|| format!("read {}", path.display()))?;
    let config = LocalConfig { path, toml };
    ctx.show(&config, render)?;
    Ok(0)
}

fn render(config: &LocalConfig) {
    outln!("{}", config.path.display());
    match &config.toml {
        Some(text) => out!("{text}"),
        None => eprintln!("No local config; create the file to add one"),
    }
}
