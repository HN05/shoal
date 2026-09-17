//! CLI repository registration and naming.
use std::path::PathBuf;

use anyhow::{Context as _, Result, ensure};

use crate::{
    cli::RepoCommand,
    client::{self, request},
    context::Context,
    protocol::Method,
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
            let config = request!(&ctx.paths, method, RepositoryConfig);
            ctx.show(&config, |config| {
                if clear {
                    println!("Cleared local repository config");
                } else if updating {
                    println!("Saved local repository config");
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
            let repo = request!(
                &ctx.paths,
                Method::RegisterRepository { source, name, path },
                Repository
            );
            ctx.emit(
                &format!("Registered {}", ui::repository_label(&repo)),
                &repo,
            )?;
        }
        RepoCommand::List => {
            let repos = client::repositories(&ctx.paths).await?;
            ctx.show(&repos, |repos| {
                for repo in repos {
                    println!("{}", ui::repository_label(repo));
                }
            })?;
        }
        RepoCommand::Rename { repository, name } => {
            let repository = ui::repository_selector(repository)?;
            let repo = request!(
                &ctx.paths,
                Method::RenameRepository { repository, name },
                Repository
            );
            ctx.emit(&format!("Renamed {}", ui::repository_label(&repo)), &repo)?;
        }
        RepoCommand::Rm { repository, yes } => {
            if !yes {
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
            let result = request!(
                &ctx.paths,
                Method::RemoveRepository { repository },
                RepositoryRemoved
            );
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

/// Expand `~` and resolve relative clone paths against the caller's directory.
fn absolute(ctx: &Context, path: PathBuf) -> Result<PathBuf> {
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
