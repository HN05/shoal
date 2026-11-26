//! The spaces `shoal move` takes.
use crate::cli::ui::Acting;
use crate::transfer::{MoveDestination, MoveOrder};
use anyhow::{Context as _, Result, ensure};

use crate::cli::{args::transfer::MoveArgs, context::Context, ui};
use crate::model::Space;
use shoal_ids::space_ids::SpaceId;
use shoal_protocol::protocol::{ErrorCode, RemoteError};
use shoal_protocol::space_names::SpaceSelector;

/// A selector that selects no space is refused before anything else is asked.
pub(super) async fn select(ctx: &Context, args: &MoveArgs, removed: bool) -> Result<Vec<SpaceId>> {
    let ids = match &args.repository {
        Some(repository) => (repository_spaces(ctx, repository.clone())
            .await?
            .into_iter())
        .map(|space| space.id)
        .collect(),
        None if !args.space.is_empty() => named(ctx, &args.space, removed).await?,
        None => picked(ctx).await?,
    };
    Ok(ids)
}

async fn named(ctx: &Context, names: &[SpaceSelector], removed: bool) -> Result<Vec<SpaceId>> {
    let mut ids = Vec::new();
    for selector in names {
        let id = live_or_removed(ctx, selector, removed).await?;
        if !ids.contains(&id) {
            ids.push(id);
        }
    }
    Ok(ids)
}

async fn live_or_removed(
    ctx: &Context,
    selector: &SpaceSelector,
    removed: bool,
) -> Result<SpaceId> {
    let located = crate::client::spaces::locate(&ctx.paths, selector.clone()).await;
    let missing = match located {
        Ok(located) => return Ok(located.id().clone()),
        Err(error) if removed && names_no_space(&error) => error,
        Err(error) => return Err(error),
    };
    let record = crate::client::removal::locate_removed(&ctx.paths, selector.clone());
    Ok((record.await).context(missing)?.space().space_id.clone())
}

fn names_no_space(error: &anyhow::Error) -> bool {
    (error.downcast_ref::<RemoteError>()).is_some_and(|remote| remote.code == ErrorCode::NotFound)
}

async fn picked(ctx: &Context) -> Result<Vec<SpaceId>> {
    let mut ids = Vec::new();
    for selector in ui::select_spaces(ctx, Vec::new(), true, &moving()).await? {
        let located = crate::client::spaces::locate(&ctx.paths, selector).await?;
        ids.push(located.id().clone());
    }
    Ok(ids)
}

async fn repository_spaces(ctx: &Context, repository: String) -> Result<Vec<Space>> {
    let repositories = crate::client::repositories::list(&ctx.paths).await?;
    let repo =
        crate::repository::select(&repositories, ui::repository_selector(repository)?).await?;
    let listed = crate::client::spaces::list(&ctx.paths).await?;
    let mut spaces: Vec<Space> = (listed.0.into_iter())
        .filter(|space| space.repository_id.as_ref() == Some(&repo.id))
        .filter(|space| space.worktree().is_some() || space.running)
        .collect();
    ensure!(
        !spaces.is_empty(),
        "{} has no spaces to move",
        repo.display_name()
    );
    spaces.sort_by(|a, b| a.name.cmp(&b.name));
    Ok(spaces)
}

/// Its destination, which may be asked for next, decides nothing of which
/// spaces the move reaches.
pub(in crate::cli::commands) fn moving() -> Acting {
    Acting::request(|_| {
        crate::client::operations::move_request(MoveOrder {
            spaces: Vec::new(),
            destination: MoveDestination::Held,
            recursive: true,
            keep: false,
            checkpoint: None,
            consent: None,
            check_only: false,
        })
    })
}
