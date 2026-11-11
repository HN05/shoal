//! Which workspaces a stop takes, and refusing them before any stops.
use crate::client::space_ids::SpaceId;
use anyhow::{Context as _, Result, ensure};

use crate::client::space_names::SpaceSelector;
use crate::{
    cli::context::Context,
    model::{SpaceKind, Workspace},
};

/// The workspaces `selectors` name, by ID, each followed by the
/// workspaces below it, through spaces of either kind, which a stop of it
/// stops too: their agents work for it.
pub(super) async fn with_children(
    ctx: &Context,
    selectors: Vec<SpaceSelector>,
) -> Result<Vec<SpaceSelector>> {
    let spaces = crate::client::workspaces::spaces(&ctx.paths).await?;
    let mut found: Vec<SpaceId> = Vec::new();
    for selector in selectors {
        let id = crate::client::workspaces::inspect(&ctx.paths, selector.clone())
            .await?
            .workspace
            .id;
        let below = spaces.below(&id).into_iter();
        let workspaces = below.filter(|space| space.kind == SpaceKind::Workspace);
        for id in std::iter::once(id).chain(workspaces.map(|space| space.id)) {
            if !found.contains(&id) {
                found.push(id);
            }
        }
    }
    Ok(found.into_iter().map(SpaceSelector::Id).collect())
}

/// Refuse a stop in a scoped process, a workspace's or a chat's agent, that
/// `selected` includes its own workspace in. The daemon refuses each
/// workspace not below the caller's space, which needs the scope token;
/// refusing its own here, before any stops, keeps the stop from taking the
/// workspaces below it alone.
pub(in crate::cli::commands) async fn ensure_below(
    ctx: &Context,
    selected: &[SpaceSelector],
) -> Result<()> {
    const REFUSAL: &str =
        "scoped processes stop only the workspaces below theirs; finish their own with shoal done";
    let own = crate::env::own_space().context(REFUSAL)?;
    ensure!(crate::env::is_scoped(), "{REFUSAL}");
    for selector in selected {
        let id = crate::client::workspaces::inspect(&ctx.paths, selector.clone())
            .await?
            .workspace
            .id;
        ensure!(id != own, "{REFUSAL}");
    }
    Ok(())
}

/// The workspaces `selectors` name, refusing them all before any stops when
/// one is unknown or stopping it would be refused now, as while another
/// lifecycle operation or a resource hook runs.
pub(super) async fn inspect_each(
    ctx: &Context,
    selectors: &[SpaceSelector],
) -> Result<Vec<Workspace>> {
    let mut workspaces = Vec::new();
    let mut refusals = Vec::new();
    for selector in selectors {
        let workspace = match crate::client::workspaces::inspect(&ctx.paths, selector.clone()).await
        {
            Ok(inspection) => inspection.workspace,
            Err(error) => {
                refusals.push(format!("{selector}: {error:#}"));
                continue;
            }
        };
        match crate::client::stopping::check(&ctx.paths, SpaceSelector::id(&workspace.id)).await {
            Ok(()) => workspaces.push(workspace),
            Err(error) => refusals.push(format!("{}: {error:#}", workspace.name)),
        }
    }
    ensure!(
        refusals.is_empty(),
        "cannot stop these workspaces, so none was stopped:\n  {}",
        refusals.join("\n  ")
    );
    Ok(workspaces)
}
