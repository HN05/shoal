//! `shoal unmerge`: the operation that undoes a workspace's local merge.
use anyhow::Result;

use crate::client::space_names::SpaceSelector;
use crate::{
    cli::{
        commands::{approvals, operations},
        context::Context,
        ui::{self, Fallback},
    },
    client::approvals::Undecided,
    model::Unmerged,
    protocol::operations::OperationSpec,
};

/// Undo the latest local merge of `workspace`'s branch into the default branch.
pub(in crate::cli::commands) async fn unmerge(
    ctx: &Context,
    workspace: Option<SpaceSelector>,
) -> Result<i32> {
    let workspace = ui::select_workspace(ctx, workspace, Fallback::CurrentDirectory).await?;
    match operations::run(
        ctx,
        OperationSpec::Unmerge {
            workspace: workspace.clone(),
        },
    )
    .await
    {
        Ok(unmerged) => {
            show_unmerged(ctx, &unmerged)?;
            Ok(0)
        }
        Err(error) => match error.downcast::<Undecided>() {
            Ok(Undecided(request)) => approvals::declined(ctx, &request),
            Err(error) => Err(error),
        },
    }
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
