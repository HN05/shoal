//! Which spaces a stop takes, and refusing them before any stops.
use anyhow::{Context as _, Result, ensure};

use crate::{
    cli::{args::SpaceTargets, commands::space_selection, context::Context},
    model::Space,
};
use shoal_protocol::model::Located;
use shoal_protocol::space_names::SpaceSelector;

/// With `children`, each is followed by the spaces below it, since their agents
/// work for it.
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

/// The daemon refuses each space not below the caller's by its scope token.
/// Refusing its own here, before any stops, keeps the stop from taking the
/// spaces below it alone.
pub(in crate::cli::commands) fn ensure_below(selected: &[Space]) -> Result<()> {
    let own = ensure_scoped()?;
    ensure!(selected.iter().all(|space| space.id != own), "{REFUSAL}");
    Ok(())
}

/// The daemon would take a process that cleared its scope token for the user.
pub(super) fn ensure_scoped() -> Result<shoal_ids::space_ids::SpaceId> {
    let own = crate::env::own_space().context(REFUSAL)?;
    ensure!(crate::env::is_scoped(), "{REFUSAL}");
    Ok(own)
}

const REFUSAL: &str =
    "scoped processes stop only the spaces below theirs; finish their own with shoal done";

/// Refuse all before any stops when stopping one would be refused now, as while
/// another lifecycle operation runs in a workspace.
pub(super) async fn check_each(ctx: &Context, spaces: &[Space]) -> Result<()> {
    let targets =
        (spaces.iter()).map(|space| (space.name.to_string(), SpaceSelector::id(&space.id)));
    check_targets(ctx, targets.collect()).await
}

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

/// The hub holds a stop for an offline machine's spaces, as that machine last
/// reported them.
pub(super) async fn on_machines(ctx: &Context, targets: SpaceTargets) -> Result<Vec<Located>> {
    let located = space_selection::located(ctx, targets, false, &super::stopping()).await?;
    let here = match located
        .iter()
        .any(|located| matches!(located, Located::Here(_)))
    {
        true => crate::client::stopping::targets(&ctx.paths).await?,
        false => Vec::new(),
    };
    Ok((located.into_iter())
        .filter(|located| match located {
            Located::Here(space) => here.iter().any(|target| target.id == space.id),
            Located::Elsewhere { space, .. } => space.space.stoppable,
        })
        .collect())
}

pub(super) fn here(located: &[Located]) -> Vec<Space> {
    (located.iter())
        .filter_map(|located| match located {
            Located::Here(space) => Some((**space).clone()),
            Located::Elsewhere { .. } => None,
        })
        .collect()
}

/// A stop for an offline machine waits for it unchecked.
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
