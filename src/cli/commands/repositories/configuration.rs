//! Read and replace locally stored repository configuration.
use std::path::PathBuf;

use anyhow::{Context as _, Result};

use super::selection::chosen;
use crate::{
    cli::{
        client::request,
        context::Context,
        output::{Palette, Style},
    },
    config::repo::LocalConfig,
    protocol::RepositoryMethod,
    repository,
};

pub(super) async fn configure(
    ctx: &Context,
    repository: Option<String>,
    file: Option<PathBuf>,
    clear: bool,
) -> Result<i32> {
    let repository = chosen(ctx, repository).await?;
    let updating = file.is_some() || clear;
    let method = config_request(repository, file, updating)?;
    let config = request::<LocalConfig>(&ctx.paths, method).await?;
    ctx.show(&config, |config| render(ctx, config, updating, clear))?;
    Ok(0)
}

fn config_request(
    repository: repository::Selector,
    file: Option<PathBuf>,
    updating: bool,
) -> Result<RepositoryMethod> {
    Ok(if updating {
        let toml = file
            .map(|path| {
                std::fs::read_to_string(&path).with_context(|| format!("read {}", path.display()))
            })
            .transpose()?;
        RepositoryMethod::SetRepositoryConfig { repository, toml }
    } else {
        RepositoryMethod::RepositoryConfig { repository }
    })
}

fn render(ctx: &Context, config: &LocalConfig, updating: bool, clear: bool) {
    if clear {
        outln!(
            "{}",
            Palette::stdout(ctx.json).paint(Style::Success, "Cleared local repository config")
        );
    } else if updating {
        outln!(
            "{}",
            Palette::stdout(ctx.json).paint(Style::Success, "Saved local repository config")
        );
    } else if let Some(text) = &config.toml {
        out!("{text}");
    } else {
        eprintln!("No local repository config; using each worktree's repository config");
    }
}
