//! The workspace `--copy` and `--checkpoint` start a new one from.
use anyhow::{Context as _, Result};
use shoal_ids::repository_ids::RepositoryId;
use shoal_ids::space_ids::SpaceId;

use crate::{
    cli::{
        args::create::CopyArgs,
        context::Context,
        ui::{self, Fallback},
    },
    protocol::CopySource,
};
use shoal_protocol::launches::{Launch, SpaceChoice, StartTarget};
use shoal_protocol::space_names::SpaceSelector;

/// What a start or split copies.
pub(super) struct Copied {
    pub source: CopySource,
    /// The ID of the copied workspace's repository.
    pub repository: RepositoryId,
}

impl Copied {
    /// `launch` copying this workspace, which a picker may have picked, in
    /// its repository unless the start names one, as a number does beside a
    /// branch or issue.
    pub(super) fn copy_into(&self, launch: &mut Launch) -> Result<()> {
        launch.copy = Some(SpaceChoice::Space(self.source.workspace.clone()));
        let positional = crate::launches::decided_target(launch)?;
        let names_repository =
            launch.repository.is_some() || matches!(positional, Some(StartTarget::Repository(_)));
        if !names_repository {
            launch.repository = Some(self.repository.to_string());
        }
        Ok(())
    }
}

/// What `args` copy; `None` when they ask for no copy. The workspace is the
/// one `--copy` selects, by number or ID, else the current one, else
/// one picked; only with `--checkpoint` may it be a removed one.
pub(super) async fn source(ctx: &Context, args: CopyArgs) -> Result<Option<Copied>> {
    if !args.is_set() {
        return Ok(None);
    }
    let selector = ui::select_workspace(
        ctx,
        args.copy.flatten(),
        Fallback::CurrentDirectory,
        &crate::cli::ui::Acting::Unconverted,
    )
    .await?;
    let workspaces = crate::client::spaces::list(&ctx.paths)
        .await?
        .into_workspaces();
    let live = (workspaces.into_iter())
        .find(|workspace| selector.selects(&workspace.id, workspace.number));
    let (workspace, repository) = match live {
        Some(workspace) => (workspace.id.clone(), workspace.repository().clone()),
        None if args.checkpoint.is_some() => removed(ctx, selector).await?,
        None => anyhow::bail!("no workspace {selector}"),
    };
    let source = CopySource {
        workspace: SpaceSelector::id(&workspace),
        checkpoint: args.checkpoint,
    };
    Ok(Some(Copied { source, repository }))
}

/// The ID of the removed workspace `selector` selects by ID, and of its
/// repository.
async fn removed(ctx: &Context, selector: SpaceSelector) -> Result<(SpaceId, RepositoryId)> {
    let removed = crate::client::removal::removed(&ctx.paths).await?;
    let removed = removed
        .into_iter()
        .find(|removed| selector.given_id() == Some(&removed.space_id))
        .with_context(|| format!("no workspace or removed workspace {selector}"))?;
    let repository = removed.repository()?.clone();
    Ok((removed.space_id, repository))
}
