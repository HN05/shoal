//! Which spaces a stop takes, and refusing them before any stops.
use anyhow::{Context as _, Result, ensure};

use crate::client::space_names::SpaceSelector;
use crate::{cli::context::Context, model::PresentSpace};

/// The spaces, workspaces or chats, that `selectors` name, each once, and
/// with `children` each followed by the spaces below it, which a stop of it
/// stops too: their agents work for it.
pub(in crate::cli::commands) async fn resolve(
    ctx: &Context,
    selectors: Vec<SpaceSelector>,
    children: bool,
) -> Result<Vec<PresentSpace>> {
    let spaces = crate::client::workspaces::spaces(&ctx.paths).await?;
    let mut found: Vec<PresentSpace> = Vec::new();
    for selector in selectors {
        let space = (spaces.find(&selector)).with_context(|| format!("no space {selector}"))?;
        let below = match children {
            true => spaces.below(space.id()),
            false => Vec::new(),
        };
        let below = (below.iter()).filter_map(|below| spaces.find(&SpaceSelector::id(&below.id)));
        for space in std::iter::once(space).chain(below) {
            if !found.iter().any(|known| known.id() == space.id()) {
                found.push(space);
            }
        }
    }
    Ok(found)
}

/// Refuse a stop in a scoped process, a workspace's or a chat's agent, that
/// `selected` includes its own space in. The daemon refuses each space not
/// below the caller's, which needs the scope token; refusing its own here,
/// before any stops, keeps the stop from taking the spaces below it alone.
pub(in crate::cli::commands) fn ensure_below(selected: &[PresentSpace]) -> Result<()> {
    const REFUSAL: &str =
        "scoped processes stop only the spaces below theirs; finish their own with shoal done";
    let own = crate::env::own_space().context(REFUSAL)?;
    ensure!(crate::env::is_scoped(), "{REFUSAL}");
    ensure!(selected.iter().all(|space| *space.id() != own), "{REFUSAL}");
    Ok(())
}

/// Refuse `spaces` all before any stops when stopping one would be refused
/// now, as while another lifecycle operation or a resource hook runs in a
/// workspace, or a restart in a chat.
pub(super) async fn check_each(ctx: &Context, spaces: &[PresentSpace]) -> Result<()> {
    let mut refusals = Vec::new();
    for space in spaces {
        let checked = crate::client::stopping::check(&ctx.paths, SpaceSelector::id(space.id()));
        if let Err(error) = checked.await {
            refusals.push(format!("{}: {error:#}", space.name()));
        }
    }
    ensure!(
        refusals.is_empty(),
        "cannot stop these spaces, so none was stopped:\n  {}",
        refusals.join("\n  ")
    );
    Ok(())
}
