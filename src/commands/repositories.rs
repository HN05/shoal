//! CLI repository registration and naming.
use std::path::PathBuf;

use anyhow::{Context as _, Result, ensure};

use crate::{
    cli::RepoCommand,
    client::{self, request},
    context::Context,
    model::{Repository, RepositoryRemoval},
    output::{Palette, Style},
    protocol::Method,
    repo_config::LocalConfig,
    ui,
};

pub(super) async fn run(ctx: &Context, command: RepoCommand) -> Result<i32> {
    match command {
        RepoCommand::Config {
            repository,
            file,
            clear,
        } => {
            let repository = ui::repository_selector(repository)?;
            let updating = file.is_some() || clear;
            let method = if updating {
                let toml = file
                    .map(|path| {
                        std::fs::read_to_string(&path)
                            .with_context(|| format!("read {}", path.display()))
                    })
                    .transpose()?;
                Method::SetRepositoryConfig { repository, toml }
            } else {
                Method::RepositoryConfig { repository }
            };
            let config = request::<LocalConfig>(&ctx.paths, method).await?;
            ctx.show(&config, |config| {
                if clear {
                    println!(
                        "{}",
                        Palette::stdout(ctx.json)
                            .paint(Style::Success, "Cleared local repository config")
                    );
                } else if updating {
                    println!(
                        "{}",
                        Palette::stdout(ctx.json)
                            .paint(Style::Success, "Saved local repository config")
                    );
                } else if let Some(text) = &config.toml {
                    print!("{text}");
                } else {
                    eprintln!(
                        "No local repository config; using each worktree's repository config"
                    );
                }
            })?;
        }
        RepoCommand::Add { source, name, path } => {
            let source = ui::repository_selector(source)?;
            let path = path.map(|path| absolute(ctx, path)).transpose()?;
            let repo = ctx
                .progress(
                    "Registering repository",
                    request::<Repository>(
                        &ctx.paths,
                        Method::RegisterRepository { source, name, path },
                    ),
                )
                .await?;
            ctx.emit(
                &format!(
                    "Registered {}",
                    ui::repository_label(&repo, Palette::stdout(ctx.json))
                ),
                &repo,
            )?;
        }
        RepoCommand::List => {
            let repos = client::repositories(&ctx.paths).await?;
            ctx.show(&repos, |repos| {
                let palette = Palette::stdout(ctx.json);
                for repo in repos {
                    println!("{}", ui::repository_label(repo, palette));
                }
            })?;
        }
        RepoCommand::Rename { repository, name } => {
            let repository = ui::repository_selector(repository)?;
            let repo =
                request::<Repository>(&ctx.paths, Method::RenameRepository { repository, name })
                    .await?;
            ctx.emit(
                &format!(
                    "Renamed {}",
                    ui::repository_label(&repo, Palette::stdout(ctx.json))
                ),
                &repo,
            )?;
        }
        RepoCommand::Rm {
            repository,
            confirmation,
        } => {
            if !confirmation.yes {
                ensure!(
                    ui::confirm(
                        ctx,
                        &format!(
                            "Delete repository: {repository}\nDeletes: checkout, all Shoal workspaces and their resources\nWork:    uncommitted and unpushed changes are permanently lost"
                        ),
                        "--yes",
                    )?,
                    "repository removal canceled"
                );
            }
            let repository = ui::repository_selector(repository)?;
            let result = ctx
                .progress(
                    "Removing repository",
                    request::<RepositoryRemoval>(
                        &ctx.paths,
                        Method::RemoveRepository { repository },
                    ),
                )
                .await?;
            ctx.emit(
                &format!(
                    "Deleted {} and {} Shoal workspaces",
                    result.path.display(),
                    result.workspaces_removed
                ),
                &result,
            )?;
        }
    }
    Ok(0)
}

/// Expand `~` and resolve relative paths against the caller's directory.
pub(super) fn absolute(ctx: &Context, path: PathBuf) -> Result<PathBuf> {
    let path = match path.strip_prefix("~") {
        Ok(relative) => ctx.paths.home.join(relative),
        Err(_) => path,
    };
    Ok(if path.is_absolute() {
        path
    } else {
        std::env::current_dir()?.join(path)
    })
}
