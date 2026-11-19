//! A local merge: the operation that merges a workspace's branch into the
//! default branch.
use anyhow::Result;

use crate::{
    cli::{args::merge::MergeArgs, commands::operations, context::Context, ui},
    env,
    forge::items::action::MergeMethod,
    model::{MergeConfirmation, MergeOptions, MergedBranch},
    protocol::{ConfigTarget, operations::OperationSpec},
};
use shoal_protocol::space_names::SpaceSelector;

/// Merge `workspace`'s branch into the default branch by the method and
/// with the message `args` give. Their `--push` or `--no-push` overrides
/// `[merge] push`, and `--yes` skips the confirmation a merge asks for when
/// it pushes or brings the default branch sensitive files it changed too.
pub(super) async fn merge(
    ctx: &Context,
    workspace: SpaceSelector,
    args: &MergeArgs,
) -> Result<i32> {
    let settings =
        shoal_workers::settings::of(&ctx.paths, ConfigTarget::Space(workspace.clone())).await?;
    let mut options = MergeOptions {
        push: args.push().unwrap_or(settings.merge.push),
        method: args.method,
        message: args.message.clone(),
        confirmed: None,
    };
    options.confirmed = match args.confirmation.yes {
        true => None,
        // An agent's merge asks the user through its approval instead, which
        // names the files the agent's --yes passed over.
        false => {
            let pushing = match options.push {
                false => Pushing::No,
                true if settings.merge.confirm && !env::is_scoped() => Pushing::AskingFirst,
                true => Pushing::WithoutAsking,
            };
            confirm_merge(ctx, &workspace, &options, pushing).await?
        }
    };
    let spec = OperationSpec::Merge {
        workspace: workspace.clone(),
        options,
    };
    let merged = operations::run(ctx, spec).await?;
    show_merged(ctx, &merged)?;
    Ok(0)
}

/// Whether a merge pushes the default branch, and whether it asks the user
/// before it does.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Pushing {
    No,
    AskingFirst,
    WithoutAsking,
}

/// Ask before merging `workspace`, showing what the merge does, when
/// `pushing` asks before it pushes, or when it brings the default branch
/// sensitive files it changed too; return what the user confirmed, if
/// asked. The daemon checks the merge first, so a merge it refuses or the
/// user declines changes nothing.
async fn confirm_merge(
    ctx: &Context,
    workspace: &SpaceSelector,
    options: &MergeOptions,
    pushing: Pushing,
) -> Result<Option<MergeConfirmation>> {
    let preview =
        crate::client::workspaces::preview_merge(&ctx.paths, workspace.clone(), options).await?;
    if pushing != Pushing::AskingFirst && preview.warning.is_none() {
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
