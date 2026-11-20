//! The spaces `shoal move` takes: those named, every space of a repository,
//! or the current one or those the picker offers.
use anyhow::{Context as _, Result, ensure};

use crate::cli::{args::transfer::MoveArgs, context::Context, ui};
use crate::model::Space;
use shoal_ids::space_ids::SpaceId;
use shoal_protocol::space_names::SpaceSelector;

/// The spaces `args` selects, by ID, refusing one that selects no space, or
/// with `files` no live or removed space, before asking for anything else.
pub(super) async fn select(ctx: &Context, args: &MoveArgs, files: bool) -> Result<Vec<SpaceId>> {
    let ids = match &args.repository {
        Some(repository) => (repository_spaces(ctx, repository.clone())
            .await?
            .into_iter())
        .map(|space| space.id)
        .collect(),
        None if !args.space.is_empty() => named(ctx, &args.space, files).await?,
        None => picked(ctx).await?,
    };
    Ok(ids)
}

/// The IDs of the spaces `names` select, or with `files` of the removed
/// spaces they select by ID.
async fn named(ctx: &Context, names: &[SpaceSelector], files: bool) -> Result<Vec<SpaceId>> {
    let mut ids = Vec::new();
    for selector in names {
        let present = crate::client::workspaces::present_space(&ctx.paths, selector).await?;
        let id = match present {
            Some(space) => space.id,
            None if files => {
                let removed = crate::client::removal::removed_space(&ctx.paths, selector.clone());
                removed
                    .await
                    .with_context(|| format!("no space {selector}"))?
                    .space_id
            }
            None => anyhow::bail!("no space {selector}"),
        };
        if !ids.contains(&id) {
            ids.push(id);
        }
    }
    Ok(ids)
}

/// The IDs of the current space or those the user picks.
async fn picked(ctx: &Context) -> Result<Vec<SpaceId>> {
    let mut ids = Vec::new();
    for selector in ui::select_spaces(ctx, Vec::new(), true).await? {
        let space = crate::client::workspaces::present_space(&ctx.paths, &selector)
            .await?
            .with_context(|| format!("no space {selector}"))?;
        ids.push(space.id);
    }
    Ok(ids)
}

/// The spaces of `repository`: its workspaces and running chats.
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
