//! The workspace `--copy` and `--checkpoint` start a new one from.
use crate::cli::ui::Acting;
use anyhow::Result;
use shoal_ids::checkpoint_numbers::CheckpointNumber;
use shoal_ids::repository_ids::RepositoryId;

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
    let checkpoint = args.checkpoint;
    let selector = ui::select_workspace(
        ctx,
        args.copy.flatten(),
        Fallback::CurrentDirectory(&copying(checkpoint)),
    )
    .await?;
    let source = CopySource {
        workspace: selector,
        checkpoint,
    };
    let origin = crate::client::workspaces::copy_origin(source)
        .send(&ctx.paths)
        .await?;
    let source = CopySource {
        workspace: SpaceSelector::id(&origin.workspace),
        checkpoint,
    };
    Ok(Some(Copied {
        source,
        repository: origin.repository,
    }))
}

/// What copying a workspace, or with `checkpoint` one of its checkpoints,
/// sends for the workspace a picker picks; the new space copies it on this
/// machine.
fn copying(checkpoint: Option<CheckpointNumber>) -> Acting {
    Acting::here(move |workspace| {
        crate::client::workspaces::copy_origin(CopySource {
            workspace,
            checkpoint,
        })
    })
}
