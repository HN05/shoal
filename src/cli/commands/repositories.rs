//! CLI repository registration and naming.
use std::path::PathBuf;

use anyhow::{Context as _, Result, ensure};

use crate::{
    cli::{
        RepoCommand,
        client::{self, request},
        context::Context,
        output::{Palette, Style},
        ui,
    },
    config::repo::LocalConfig,
    forge::repository,
    model::{Repository, RepositoryRemoval, SyncedRepository},
    protocol::Method,
};

pub(super) async fn run(ctx: &Context, command: RepoCommand) -> Result<i32> {
    match command {
        RepoCommand::Config {
            repository,
            file,
            clear,
        } => {
            let repository = chosen(ctx, repository).await?;
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
            let repo = register(ctx, source, name, path).await?;
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
            let repository = chosen(ctx, repository).await?;
            let name = match name {
                Some(name) => name,
                None => ui::input(ctx, "New name")?,
            };
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
            let mut repository = chosen(ctx, repository).await?;
            if !confirmation.yes {
                let repositories = client::repositories(&ctx.paths).await?;
                let repo = repository::select(&repositories, &repository).await?;
                ensure!(
                    ui::confirm(
                        ctx,
                        &format!(
                            "Delete repository: {}\nSource: {}\nCheckout: {}\nDeletes: checkout, all Shoal workspaces and their resources\nWork:    uncommitted and unpushed changes are permanently lost",
                            repository::name(repo),
                            repo.source,
                            repo.path.display(),
                        ),
                        "--yes",
                    )?,
                    "repository removal canceled"
                );
                // A rename while the prompt is open must not redirect deletion.
                repository = repo.id.clone().into();
            }
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

/// The named repository, else one chosen from the registered repositories.
async fn chosen(ctx: &Context, repository: Option<String>) -> Result<repository::Selector> {
    match repository {
        Some(repository) => ui::repository_selector(repository),
        None => {
            let repos = client::repositories(&ctx.paths).await?;
            Ok(ui::pick(ctx, "Repository> ", ui::repository_choices(repos).await?)?.into())
        }
    }
}

pub(super) async fn sync(ctx: &Context, repository: Option<String>) -> Result<i32> {
    let repository = match repository {
        Some(repository) => ui::repository_selector(repository)?,
        None => super::issues::current_repository(
            ctx,
            client::repositories(&ctx.paths).await?,
            "pass a repository",
        )
        .await?
        .into(),
    };
    let synced =
        request::<SyncedRepository>(&ctx.paths, Method::SyncRepository { repository }).await?;
    ctx.show(&synced, |synced| {
        if let Some(remote) = &synced.remote {
            println!("Fetched {remote}");
        }
        let refresh = &synced.default_branch;
        match (&refresh.skipped, refresh.updated) {
            (Some(skipped), _) => println!("{skipped}"),
            (None, true) => println!(
                "Updated {} from its upstream ({}..{})",
                refresh.branch, refresh.previous_commit, refresh.commit
            ),
            (None, false) => println!("{} is up to date with its upstream", refresh.branch),
        }
    })?;
    Ok(0)
}

async fn register(
    ctx: &Context,
    source: repository::Selector,
    name: Option<String>,
    path: Option<PathBuf>,
) -> Result<Repository> {
    ctx.progress(
        "Registering repository",
        request::<Repository>(
            &ctx.paths,
            Method::RegisterRepository { source, name, path },
        ),
    )
    .await
}

/// Register `source` for a command that needs it, when the user agrees.
pub(super) async fn offer_registration(ctx: &Context, source: &str) -> Result<Option<Repository>> {
    if !ui::offer(ctx, &format!("Register {source} with Shoal?"))? {
        return Ok(None);
    }
    let repo = register(ctx, ui::repository_selector(source.to_owned())?, None, None).await?;
    eprintln!(
        "Registered {}",
        ui::repository_label(&repo, Palette::stderr(ctx.json))
    );
    Ok(Some(repo))
}

/// Offer to register an explicit checkout or URL that matches no registered
/// repository, so the command can continue with the new registration.
pub(super) async fn offer_unregistered(
    ctx: &Context,
    selector: &repository::Selector,
) -> Result<Option<Repository>> {
    if !ctx.interactive() || !repository::registrable(selector.source()?) {
        return Ok(None);
    }
    let repos = client::repositories(&ctx.paths).await?;
    match repository::select(&repos, selector).await {
        Err(error) if error.is::<repository::NotRegistered>() => {
            match offer_registration(ctx, selector.source()?).await? {
                Some(repo) => Ok(Some(repo)),
                None => Err(error),
            }
        }
        // Other selection failures surface where the selector is used.
        _ => Ok(None),
    }
}

/// Expand `~` and resolve relative paths against the caller's directory.
pub(super) fn absolute(ctx: &Context, path: PathBuf) -> Result<PathBuf> {
    let path = crate::fsutil::expand_home(&path, &ctx.paths.home);
    Ok(if path.is_absolute() {
        path
    } else {
        std::env::current_dir()?.join(path)
    })
}
