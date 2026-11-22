//! Which spaces a stop takes, and refusing them before any stops.
use anyhow::{Context as _, Result, ensure};

use crate::{cli::context::Context, model::Space};
use shoal_protocol::model::Located;
use shoal_protocol::space_names::SpaceSelector;

/// The spaces, workspaces or chats, that `selectors` name, each once, and
/// with `children` each followed by the spaces below it, which a stop of it
/// stops too: their agents work for it.
pub(in crate::cli::commands) async fn resolve(
    ctx: &Context,
    selectors: Vec<SpaceSelector>,
    children: bool,
) -> Result<Vec<Space>> {
    let spaces = crate::client::spaces::list(&ctx.paths).await?;
    let mut found: Vec<Space> = Vec::new();
    for selector in selectors {
        let space = (spaces.find(&selector)).with_context(|| format!("no space {selector}"))?;
        let below = match children {
            true => spaces.below(&space.id),
            false => Vec::new(),
        };
        let below = (below.iter()).filter_map(|below| spaces.find(&SpaceSelector::id(&below.id)));
        for space in std::iter::once(space).chain(below) {
            if !found.iter().any(|known| known.id == space.id) {
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
pub(in crate::cli::commands) fn ensure_below(selected: &[Space]) -> Result<()> {
    let own = ensure_scoped()?;
    ensure!(selected.iter().all(|space| space.id != own), "{REFUSAL}");
    Ok(())
}

/// Refuse a stop in a process of a space that cleared its scope token,
/// which the daemon would take for the user's, returning its space.
pub(super) fn ensure_scoped() -> Result<shoal_ids::space_ids::SpaceId> {
    let own = crate::env::own_space().context(REFUSAL)?;
    ensure!(crate::env::is_scoped(), "{REFUSAL}");
    Ok(own)
}

const REFUSAL: &str =
    "scoped processes stop only the spaces below theirs; finish their own with shoal done";

/// Refuse `spaces` all before any stops when stopping one would be refused
/// now, as while another lifecycle operation or a resource hook runs in a
/// workspace, or a restart in a chat.
pub(super) async fn check_each(ctx: &Context, spaces: &[Space]) -> Result<()> {
    let targets =
        (spaces.iter()).map(|space| (space.name.to_string(), SpaceSelector::id(&space.id)));
    check_targets(ctx, targets.collect()).await
}

/// [`check_each`] over `targets`, each named for its refusal, which may run
/// on another machine.
pub(super) async fn check_targets(
    ctx: &Context,
    targets: Vec<(String, SpaceSelector)>,
) -> Result<()> {
    let mut refusals = Vec::new();
    for (name, selector) in targets {
        let checked = crate::client::stopping::check(selector);
        if let Err(error) = checked.send(&ctx.paths).await {
            refusals.push(format!("{name}: {error:#}"));
        }
    }
    ensure!(
        refusals.is_empty(),
        "cannot stop these spaces, so none was stopped:\n  {}",
        refusals.join("\n  ")
    );
    Ok(())
}

/// The spaces of this machine among `located`.
pub(super) fn here(located: &[Located]) -> Vec<Space> {
    (located.iter())
        .filter_map(|located| match located {
            Located::Here(space) => Some((**space).clone()),
            Located::Elsewhere { .. } => None,
        })
        .collect()
}

/// What a stop of `located` checks before it stops any: each space here,
/// and those elsewhere on connected machines, since a stop for an offline
/// one waits for it unchecked.
pub(super) fn checked(located: &[Located]) -> Vec<(String, SpaceSelector)> {
    (located.iter())
        .filter_map(|located| match located {
            Located::Here(space) => Some((space.name.to_string(), SpaceSelector::id(&space.id))),
            Located::Elsewhere { space, machine } => machine.connected.then(|| {
                let space = &space.space;
                (space.name.to_string(), SpaceSelector::id(&space.id))
            }),
        })
        .collect()
}
