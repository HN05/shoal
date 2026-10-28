//! Register repositories and offer registration to other commands.
use std::path::PathBuf;

use anyhow::Result;

use super::selection::absolute;
use crate::{
    cli::{
        client::{self, request},
        context::Context,
        output::Palette,
        ui,
    },
    model::Repository,
    protocol::RepositoryMethod,
    repository,
};

pub(super) async fn add(
    ctx: &Context,
    source: String,
    name: Option<String>,
    path: Option<PathBuf>,
) -> Result<i32> {
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
            RepositoryMethod::RegisterRepository { source, name, path },
        ),
    )
    .await
}

/// Register `source` for a command that needs it, when the user agrees.
pub(in crate::cli::commands) async fn offer_registration(
    ctx: &Context,
    source: &str,
) -> Result<Option<Repository>> {
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
pub(in crate::cli::commands) async fn offer_unregistered(
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
