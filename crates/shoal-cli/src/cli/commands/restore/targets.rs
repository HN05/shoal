//! What `shoal restore` reopens: the removed spaces named by ID, refusing a
//! space that is not removed; without a space, those picked among the
//! removed spaces, or else a list of them.
use anyhow::{Result, bail};

use crate::client::output::restore::Restorable;
use crate::{
    cli::{context::Context, ui},
    model::RemovedSpace,
};
use shoal_protocol::space_names::SpaceSelector;

/// The removed spaces `names` names, each once and a workspace before a
/// chat in its worktree, refusing a space that is not removed; without a
/// name, those picked among the removed spaces the caller may restore.
/// `None` once it listed those, without a terminal or anything to pick.
pub(super) async fn removed(
    ctx: &Context,
    names: Vec<SpaceSelector>,
) -> Result<Option<Vec<RemovedSpace>>> {
    let spaces = crate::client::spaces::list(&ctx.paths).await?;
    if let Some(space) = names.iter().find_map(|name| spaces.find(name)) {
        let (kind, number) = (space.kind(), space.number);
        bail!("{kind} {number} is not removed; resume its agent with `shoal resume {number}`");
    }
    let removed = crate::client::removal::removed(&ctx.paths).await?;
    let mut selected = match names.is_empty() {
        true => match pick(ctx, &removed)? {
            Some(picked) => picked,
            None => {
                let restorable = Restorable { removed };
                ctx.show(&restorable, print_restorable)?;
                return Ok(None);
            }
        },
        false => named(ctx, removed, names).await?,
    };
    selected.sort_by_key(|space| space.workspace().is_none());
    Ok(Some(selected))
}

/// The removed spaces `names` names among `removed`, each once, asking the
/// daemon for one the list leaves out, which it refuses with why.
async fn named(
    ctx: &Context,
    removed: Vec<RemovedSpace>,
    names: Vec<SpaceSelector>,
) -> Result<Vec<RemovedSpace>> {
    let mut selected: Vec<RemovedSpace> = Vec::new();
    for name in names {
        let found = (removed.iter()).find(|space| name.given_id() == Some(&space.space_id));
        let space = match found {
            Some(space) => space.clone(),
            None => crate::client::removal::removed_space(&ctx.paths, name).await?,
        };
        if !selected
            .iter()
            .any(|named| named.space_id == space.space_id)
        {
            selected.push(space);
        }
    }
    Ok(selected)
}

/// The removed spaces picked among `removed`; `None` without a terminal or
/// anything to pick.
fn pick(ctx: &Context, removed: &[RemovedSpace]) -> Result<Option<Vec<RemovedSpace>>> {
    if removed.is_empty() || !ctx.interactive() {
        return Ok(None);
    }
    let entries = removed
        .iter()
        .map(|space| space.space_id.to_string())
        .zip(super::reopen::removed_lines(removed))
        .collect();
    let picked = ui::pick_many(ctx, "Spaces> ", entries)?;
    Ok(Some(
        (removed.iter())
            .filter(|space| picked.contains(&space.space_id.to_string()))
            .cloned()
            .collect(),
    ))
}

/// Print the removed spaces of `restorable` by kind, each led by its ID,
/// which selects it.
fn print_restorable(restorable: &Restorable) {
    let removed = &restorable.removed;
    if removed.is_empty() {
        outln!("No removed spaces to restore");
    }
    let lines = super::reopen::removed_lines(removed);
    let (workspaces, chats): (Vec<_>, Vec<_>) =
        (removed.iter().zip(lines)).partition(|(space, _)| space.workspace().is_some());
    for (heading, group) in [
        ("Removed workspaces:", workspaces),
        ("Removed chats:", chats),
    ] {
        if !group.is_empty() {
            outln!("{heading}");
        }
        for (space, line) in group {
            outln!("  {}  {line}", space.space_id);
        }
    }
}
