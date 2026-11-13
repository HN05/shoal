//! `shoal merge`: the operation that merges a workspace's branch.
use anyhow::Result;

use crate::client::space_names::SpaceSelector;
use crate::{
    cli::{
        commands::{approvals, operations},
        context::Context,
        ui::{self, Fallback},
    },
    client::approvals::Undecided,
    env,
    model::{MergeConfirmation, MergeOptions, MergedBranch},
    protocol::{ConfigTarget, operations::OperationSpec},
};

/// `push` overrides `[merge] push` when given, and `yes` skips the
/// confirmation a merge asks for when it pushes or brings the default branch
/// sensitive files it changed too.
pub(in crate::cli::commands) async fn merge(
    ctx: &Context,
    workspace: Option<SpaceSelector>,
    push: Option<bool>,
    yes: bool,
) -> Result<i32> {
    let workspace = ui::select_workspace(ctx, workspace, Fallback::CurrentDirectory).await?;
    let settings =
        crate::config::settings(&ctx.paths, ConfigTarget::Space(workspace.clone())).await?;
    let push = push.unwrap_or(settings.merge.push);
    let confirmed = match yes {
        true => None,
        // An agent's merge asks the user through its approval instead, which
        // names the files the agent's --yes passed over.
        false => {
            let pushing = match push {
                false => Pushing::No,
                true if settings.merge.confirm && !env::is_scoped() => Pushing::AskingFirst,
                true => Pushing::WithoutAsking,
            };
            confirm_merge(ctx, &workspace, pushing).await?
        }
    };
    let options = MergeOptions { push, confirmed };
    let spec = OperationSpec::Merge {
        workspace: workspace.clone(),
        options,
    };
    match operations::run(ctx, spec).await {
        Ok(merged) => {
            show_merged(ctx, &merged)?;
            Ok(0)
        }
        Err(error) => match error.downcast::<Undecided>() {
            Ok(Undecided(request)) => approvals::declined(ctx, &request),
            Err(error) => Err(error),
        },
    }
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
    pushing: Pushing,
) -> Result<Option<MergeConfirmation>> {
    let push = pushing != Pushing::No;
    let preview =
        crate::client::workspaces::preview_merge(&ctx.paths, workspace.clone(), push).await?;
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
        if !merged.updated {
            outln!("{default} already contains {branch} ({commit})");
        } else if merged.fast_forward {
            outln!("Fast-forwarded {default} to {branch} ({commit})");
        } else {
            outln!("Merged {branch} into {default} ({commit})");
        }
        if let Some(remote) = &merged.pushed_to {
            outln!("Pushed {default} to {remote}");
        }
    })
}
