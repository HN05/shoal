//! A PR merge: merging a PR on the forge, now or once its checks pass, or
//! withdrawing its automatic merge.
use anyhow::Result;

use crate::{
    cli::{
        args::merge::MergeArgs,
        commands::{items, merge_warning},
        context::Context,
    },
    forge::items::{action::Action, item::ItemState, link::ItemKind, reference::ItemInput},
    protocol::RepoTarget,
};

/// Merge the PR `item` names in the `target` repository, or the space's
/// linked one, as `args` ask, unless the user declines a merge the daemon
/// warns about; `--yes` merges whatever it warns about.
pub(super) async fn merge(
    ctx: &Context,
    target: RepoTarget,
    item: Option<ItemInput>,
    args: &MergeArgs,
) -> Result<i32> {
    let (action, outcome) = action(args);
    if matches!(action, Action::CancelAutoMerge) {
        return items::act_in(ctx, target, item, ItemKind::Pr, action, outcome).await;
    }
    let confirmed = match args.confirmation.yes {
        true => None,
        false => Some(merge_warning::confirm_pull(ctx, &target, item.clone()).await?),
    };
    let auto_merge = matches!(action, Action::AutoMerge { .. });
    let item = crate::client::items::merge(&ctx.paths, target, item, action, confirmed).await?;
    // The forge merges now a PR whose checks already pass.
    let outcome = match auto_merge && item.state == ItemState::Merged {
        true => "PR merged",
        false => outcome,
    };
    items::report(ctx, &item, outcome)
}

/// The merge `args` ask for, or the withdrawal of an automatic merge.
fn action(args: &MergeArgs) -> (Action, &'static str) {
    let (method, message, delete_branch) = (args.method, args.message.clone(), args.delete_branch);
    if args.cancel_auto {
        return (Action::CancelAutoMerge, "Auto-merge canceled");
    }
    match args.auto {
        true => (
            Action::AutoMerge {
                method,
                message,
                delete_branch,
                head: None,
            },
            "Auto-merge enabled",
        ),
        false => (
            Action::Merge {
                method,
                message,
                delete_branch,
                head: None,
            },
            "PR merged",
        ),
    }
}
