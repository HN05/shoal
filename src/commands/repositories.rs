//! CLI repository registration and naming.
use super::output;
use crate::cli::RepoCommand;
use crate::{
    client,
    paths::Paths,
    protocol::{Body, Method},
    ui,
};
use anyhow::{Context, Result};

pub(super) async fn run(paths: &Paths, command: RepoCommand, json_output: bool) -> Result<i32> {
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
            match client::call(paths, method).await? {
                Body::RepositoryConfig(config) => {
                    if json_output {
                        println!("{}", serde_json::to_string(&config)?);
                    } else if updating {
                        println!(
                            "{}",
                            if clear {
                                "Cleared local repository config"
                            } else {
                                "Saved local repository config"
                            }
                        );
                    } else if let Some(text) = config.toml {
                        print!("{text}");
                    } else {
                        eprintln!(
                            "No local repository config; using each worktree's repository config"
                        );
                    }
                }
                _ => anyhow::bail!("unexpected repository config response"),
            }
        }
        RepoCommand::Add { source, name, path } => {
            let source = ui::repository_selector(source)?;
            let path = path
                .map(|path| -> Result<_> {
                    let path = match path.strip_prefix("~") {
                        Ok(relative) => paths.home.join(relative),
                        Err(_) => path,
                    };
                    Ok(if path.is_absolute() {
                        path
                    } else {
                        std::env::current_dir()?.join(path)
                    })
                })
                .transpose()?;
            match client::call(paths, Method::Register { source, name, path }).await? {
                Body::Repository(repo) => output(
                    json_output,
                    &format!("Registered {}", ui::repository_label(&repo)),
                    serde_json::to_value(&repo)?,
                ),
                _ => anyhow::bail!("unexpected registration response"),
            }
        }
        RepoCommand::List => {
            let repos = ui::repositories(paths).await?;
            if json_output {
                println!("{}", serde_json::to_string(&repos)?);
            } else {
                for repo in repos {
                    println!("{}", ui::repository_label(&repo));
                }
            }
        }
        RepoCommand::Rename { repository, name } => {
            let repository = ui::repository_selector(repository)?;
            match client::call(paths, Method::RenameRepository { repository, name }).await? {
                Body::Repository(repo) => output(
                    json_output,
                    &format!("Renamed {}", ui::repository_label(&repo)),
                    serde_json::to_value(&repo)?,
                ),
                _ => anyhow::bail!("unexpected repository response"),
            }
        }
        RepoCommand::Rm { repository, yes } => {
            if !yes {
                anyhow::ensure!(
                    ui::confirm(
                        &format!(
                            "Delete repository: {repository}\nDeletes: checkout, all Shoal workspaces and their resources\nWork:    uncommitted and unpushed changes are permanently lost"
                        ),
                        json_output,
                        "--yes",
                    )?,
                    "repository removal canceled"
                );
            }
            let repository = ui::repository_selector(repository)?;
            match client::call(paths, Method::RemoveRepository { repository }).await? {
                Body::RepositoryRemoved(result) => output(
                    json_output,
                    &format!(
                        "Deleted {} and {} Shoal workspaces",
                        result.path.display(),
                        result.workspaces_removed
                    ),
                    serde_json::to_value(&result)?,
                ),
                _ => anyhow::bail!("unexpected repository removal response"),
            }
        }
    }
    Ok(0)
}
