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
    protocol::Method,
    repository,
};

/// Save `root` as `root_dir` when given, then have the daemon move every
/// repository directory outside the configured root into it.
pub(super) async fn run(ctx: &Context, root: Option<PathBuf>, confirmed: bool) -> Result<()> {
    let repositories = client::repositories(&ctx.paths).await?;
    let target = match &root {
        Some(root) => root.clone(),
        None => Config::load(&ctx.paths)?.root_dir(&ctx.paths)?,
    };
    let moving = outside(&repositories, &target);
    if !confirmed && !moving.is_empty() {
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
            request::<Vec<MovedRepository>>(&ctx.paths, Method::MoveRepositories),
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
            println!("No repository directories to move");
        }
        for moved in moved {
            let directory = moved.repository.workspaces_dir.as_deref();
            println!(
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

/// Repositories whose directory is not directly in `root`.
fn outside<'a>(repositories: &'a [Repository], root: &Path) -> Vec<&'a Repository> {
    let root = std::fs::canonicalize(root).unwrap_or_else(|_| root.to_owned());
    repositories
        .iter()
        .filter(|repo| {
            repo.workspaces_dir
                .as_deref()
                .is_some_and(|directory| directory.parent() != Some(&*root))
        })
        .collect()
}

fn plan(root: &Path, moving: &[&Repository]) -> String {
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
    text.push_str("\nStop their agents and commands, and leave these directories first.");
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
