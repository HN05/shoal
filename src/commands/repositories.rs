//! CLI repository registration and naming.
use super::output;
use crate::cli::RepoCommand;
use crate::{
    client,
    paths::Paths,
    protocol::{Body, Method},
    ui,
};
use anyhow::Result;

pub(super) async fn run(paths: &Paths, command: RepoCommand, json_output: bool) -> Result<i32> {
    match command {
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
    }
    Ok(0)
}
