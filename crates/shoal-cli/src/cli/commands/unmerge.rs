//! `shoal unmerge`: the operation that undoes a workspace's local merge.
use anyhow::Result;

use crate::{
    cli::{
        commands::operations,
        context::Context,
        ui::{self, Fallback},
    },
    model::Unmerged,
    protocol::operations::OperationSpec,
};
use shoal_protocol::space_names::SpaceSelector;

/// Undo the latest local merge of `workspace`'s branch into the default branch.
pub(in crate::cli::commands) async fn unmerge(
    ctx: &Context,
    workspace: Option<SpaceSelector>,
) -> Result<i32> {
    let workspace = ui::select_workspace(
        ctx,
        workspace,
        Fallback::CurrentDirectory(&crate::cli::ui::Acting::Unconverted),
    )
    .await?;
    let spec = OperationSpec::Unmerge { workspace };
    let unmerged = operations::run(ctx, spec).await?;
    show_unmerged(ctx, &unmerged)?;
    Ok(0)
}

fn show_unmerged(ctx: &Context, unmerged: &Unmerged) -> Result<()> {
    ctx.show(unmerged, |unmerged| {
        let (branch, default, commit) =
            (&unmerged.branch, &unmerged.default_branch, &unmerged.commit);
        match unmerged.replayed {
            0 => outln!("Moved {default} back to {commit}, before {branch} merged"),
            replayed => outln!(
                "Took {branch} out of {default} and replayed {replayed} later commit{} ({commit})",
                if replayed == 1 { "" } else { "s" }
            ),
        }
        if unmerged.branch_recreated {
            outln!("Recreated branch {branch} at {}", unmerged.source);
        }
    })
}
