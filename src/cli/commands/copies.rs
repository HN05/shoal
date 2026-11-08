//! The workspace `--copy` and `--checkpoint` start a new one from.
use crate::client::space_ids::SpaceId;
use anyhow::{Context as _, Result};

use crate::client::space_names::SpaceSelector;
use crate::{
    cli::{
        args::create::CopyArgs,
        context::Context,
        ui::{self, Fallback},
    },
    protocol::CopySource,
};

/// What a start or split copies.
pub(super) struct Copied {
    pub source: CopySource,
    /// The ID of the copied workspace's repository.
    pub repository: String,
    /// The name a copy takes without one of its own: the copied branch's,
    /// or the workspace's, with `-copy` added.
    pub name: String,
}

/// What `args` copy; `None` when they ask for no copy. The workspace is the
/// one `--copy` names, by name, ID or number, else the current one, else
/// one picked; only with `--checkpoint` may it be a removed one.
pub(super) async fn source(ctx: &Context, args: CopyArgs) -> Result<Option<Copied>> {
    if !args.is_set() {
        return Ok(None);
    }
    let selector =
        ui::select_workspace(ctx, args.copy.flatten(), Fallback::CurrentDirectory).await?;
    let workspaces = crate::client::workspaces::list(&ctx.paths).await?;
    let live = (workspaces.into_iter())
        .find(|workspace| selector.selects(&workspace.id, &workspace.name, workspace.number));
    let (workspace, repository, named) = match live {
        Some(workspace) => (
            workspace.id,
            workspace.repository_id,
            workspace.branch.unwrap_or(workspace.name.into()),
        ),
        None if args.checkpoint.is_some() => removed(ctx, selector).await?,
        None => anyhow::bail!("no workspace {selector}"),
    };
    let source = CopySource {
        workspace: SpaceSelector::id(&workspace),
        checkpoint: args.checkpoint,
    };
    Ok(Some(Copied {
        source,
        repository,
        name: format!("{named}-copy"),
    }))
}

/// The ID of the removed workspace `selector` names, by name or ID, of its
/// repository, and its branch or else its name.
async fn removed(ctx: &Context, selector: SpaceSelector) -> Result<(SpaceId, String, String)> {
    let removed = crate::client::removal::removed(&ctx.paths).await?;
    let removed = removed
        .into_iter()
        .find(|removed| selector.names(&removed.workspace_id, &removed.name))
        .with_context(|| format!("no workspace or removed workspace {selector}"))?;
    let named = removed.checkpoint.branch.unwrap_or(removed.name.into());
    Ok((removed.workspace_id, removed.repository_id, named))
}
