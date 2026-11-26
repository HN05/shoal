//! A local merge of a workspace's branch into the default branch.
use anyhow::Result;

use crate::{
    cli::{args::merge::MergeArgs, commands::operations, context::Context, ui},
    forge::items::action::MergeMethod,
    model::{MergeConfirmation, MergeOptions, MergedBranch},
    protocol::operations::OperationSpec,
};
use shoal_protocol::space_names::SpaceSelector;

pub(super) async fn merge(
    ctx: &Context,
    workspace: SpaceSelector,
    args: &MergeArgs,
) -> Result<i32> {
    let mut options = MergeOptions {
        push: args.push(),
        method: args.method,
        message: args.message.clone(),
        confirmed: None,
    };
    if !args.confirmation.yes {
        options.confirmed = confirm_merge(ctx, &workspace, &options).await?;
    }
    let spec = OperationSpec::Merge {
        workspace: workspace.clone(),
        options,
    };
    let merged = operations::run(ctx, spec).await?;
    show_merged(ctx, &merged)?;
    Ok(0)
}

/// The daemon checks the merge first, so a merge it refuses or the user
/// declines changes nothing. An agent's merge asks about its push through its
/// approval request instead.
async fn confirm_merge(
    ctx: &Context,
    workspace: &SpaceSelector,
    options: &MergeOptions,
) -> Result<Option<MergeConfirmation>> {
    let preview =
        crate::client::merging::preview_merge(&ctx.paths, workspace.clone(), options).await?;
    if !preview.asks_before_push && preview.warning.is_none() {
        return Ok(None);
    }
    anyhow::ensure!(
        ui::confirm(ctx, &preview.description(), "--yes")?,
        "merge canceled"
    );
    Ok(Some(preview.confirmation()))
}

fn show_merged(ctx: &Context, merged: &MergedBranch) -> Result<()> {
    ctx.show(merged, |merged| {
        let refresh = &merged.default_refresh;
        if refresh.updated {
            outln!(
                "Updated {} from its upstream ({}..{})",
                refresh.branch,
                refresh.previous_commit,
                refresh.commit
            );
        }
        let (branch, default, commit) = (&merged.branch, &merged.default_branch, &merged.commit);
        match merged.method {
            _ if !merged.updated => outln!("{default} already contains {branch} ({commit})"),
            MergeMethod::Squash => outln!("Squashed {branch} into {default} ({commit})"),
            MergeMethod::Rebase => outln!("Rebased {branch} onto {default} ({commit})"),
            MergeMethod::Merge if merged.fast_forward => {
                outln!("Fast-forwarded {default} to {branch} ({commit})")
            }
            MergeMethod::Merge => outln!("Merged {branch} into {default} ({commit})"),
        }
        if let Some(remote) = &merged.pushed_to {
            outln!("Pushed {default} to {remote}");
        }
    })
}
