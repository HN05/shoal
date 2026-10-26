//! Move every repository directory into a new `root_dir`.
use std::path::{Path, PathBuf};

use anyhow::{Result, ensure};

use crate::{
    cli::{
        agents,
        client::{self, request},
        context::Context,
        output::{Palette, Style},
        ui,
    },
    config::{Config, edit::Change},
    model::{MovedRepository, Repository},
    protocol::RepositoryMethod,
    repository,
};

/// Check that every repository outside `root`, or the configured root, can
/// move, confirm, save `root` as `root_dir` when given, then have the daemon
/// move them.
pub(super) async fn run(ctx: &Context, root: Option<PathBuf>, confirmed: bool) -> Result<()> {
    let repositories = client::repositories(&ctx.paths).await?;
    let moving = ctx
        .progress(
            "Checking repositories",
            request::<Vec<Repository>>(
                &ctx.paths,
                RepositoryMethod::PlanRepositoryMoves { root: root.clone() },
            ),
        )
        .await?;
    if !confirmed && !moving.is_empty() {
        let target = match &root {
            Some(root) => root.clone(),
            None => Config::load(&ctx.paths)?.root_dir(&ctx.paths)?,
        };
        ensure!(
            ui::confirm(ctx, &plan(&target, &moving), "--yes")?,
            "repository move canceled"
        );
    }
    if let Some(root) = root {
        save_root(ctx, &root).await?;
    }
    let moved = ctx
        .progress(
            "Moving repositories",
            request::<Vec<MovedRepository>>(&ctx.paths, RepositoryMethod::MoveRepositories),
        )
        .await?;
    // Agents trust the new paths when they next start there.
    for moved in &moved {
        if let Some(old) = repositories
            .iter()
            .find(|repo| repo.id == moved.repository.id)
        {
            agents::cleanup_repository_trust(ctx, old, &moved.workspace_paths).await;
        }
    }
    ctx.show(&moved, |moved| {
        let palette = Palette::stdout(ctx.json);
        if moved.is_empty() {
            outln!("No repository directories to move");
        }
        for moved in moved {
            let directory = moved.repository.workspaces_dir.as_deref();
            outln!(
                "{}",
                palette.paint(
                    Style::Success,
                    format_args!(
                        "Moved {} to {}",
                        repository::name(&moved.repository),
                        directory.unwrap_or(&moved.repository.path).display()
                    )
                )
            );
        }
    })
}

fn plan(root: &Path, moving: &[Repository]) -> String {
    let mut text = format!("Move repository directories into {}:", root.display());
    for repo in moving {
        if let Some(directory) = &repo.workspaces_dir {
            text.push_str(&format!(
                "\n  {}: {}",
                repository::name(repo),
                directory.display()
            ));
        }
    }
    text
}

/// Save `root` as the global `root_dir` and apply it to the daemon, which
/// moves repositories into the root it has loaded.
async fn save_root(ctx: &Context, root: &Path) -> Result<()> {
    let value = toml::Value::String(
        root.to_str()
            .ok_or_else(|| anyhow::anyhow!("root directory is not UTF-8"))?
            .to_owned(),
    );
    Config::edit(
        &ctx.paths,
        &[Change {
            key: "root_dir".into(),
            value: Some(value.to_string()),
        }],
    )?;
    client::reload_config(&ctx.paths).await?;
    Ok(())
}
