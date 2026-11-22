//! `shoal merge`: merge a space's work, through its open PR on the forge or
//! a workspace's branch locally into the default branch, or a PR it names.
use crate::cli::ui::Acting;
use anyhow::Result;

use crate::{
    cli::{
        args::merge::{MergeArgs, Merged},
        commands::items::select_item_target,
        context::Context,
        ui::{self, Fallback},
    },
    forge::items::{link::ItemKind, reference::ItemInput},
    model::MergePath,
    protocol::{ItemContext, RepoTarget, operations::OperationSpec},
};

mod local;
mod pull;

pub(super) async fn run(ctx: &Context, args: MergeArgs) -> Result<i32> {
    match args.merged()? {
        Merged::Pr(item) => merge_named(ctx, &args, item).await,
        Merged::Work => merge_work(ctx, &args).await,
    }
}

/// What merging as `args` ask sends for the space a picker picks; `None`
/// where they conflict, which the merge refuses before any picker.
pub(super) fn acting(args: &MergeArgs) -> Option<Acting> {
    Some(match args.merged().ok()? {
        Merged::Pr(item) => merging_pr(item, args),
        Merged::Work => merging_work(),
    })
}

/// What merging the PR `item` names, or the space's linked one, sends in
/// the context of the space a picker picks.
fn merging_pr(item: Option<ItemInput>, args: &MergeArgs) -> Acting {
    let action = pull::action(args).0;
    crate::cli::commands::items::in_context(move |target| {
        crate::client::items::act(target, ItemKind::Pr, item.clone(), action.clone())
    })
}

/// What merging a space's work sends for the space a picker picks: a
/// space without a PR merges in an operation on this machine, which its
/// worktree's branch merges into.
fn merging_work() -> Acting {
    Acting::request(|workspace| {
        let spec = OperationSpec::Merge {
            workspace,
            options: Default::default(),
        };
        crate::client::operations::start_request(spec, Default::default())
    })
}

/// Merge the PR `item` names, or the space's linked one, in the repository
/// `args` name.
async fn merge_named(ctx: &Context, args: &MergeArgs, item: Option<ItemInput>) -> Result<i32> {
    let named = match &item {
        Some(item) => format!("PR {item}"),
        None => "the space's linked PR".to_owned(),
    };
    args.ensure_pull_flags(&named)?;
    let (space, repo) = (args.space.clone(), args.repo.name.clone());
    let merging = merging_pr(item.clone(), args);
    let target = select_item_target(ctx, space, repo, item.as_ref(), &merging).await?;
    pull::merge(ctx, target, item, args).await
}

/// Merge the work of the space `args` name as the daemon decides: through
/// its open PR, or a workspace's branch locally.
async fn merge_work(ctx: &Context, args: &MergeArgs) -> Result<i32> {
    let merging = merging_work();
    let space = ui::select_space(
        ctx,
        args.space.clone(),
        Fallback::CurrentDirectory(&merging),
    )
    .await?;
    match crate::client::merging::merge_path(space.clone())
        .send(&ctx.paths)
        .await?
    {
        MergePath::Pr { pr } => {
            args.ensure_pull_flags(&format!("PR {pr}"))?;
            let target = RepoTarget {
                context: Some(ItemContext::space(space)),
                repository: None,
            };
            pull::merge(ctx, target, Some(pr.into()), args).await
        }
        MergePath::Local => {
            args.ensure_local_flags(&named_space(args))?;
            local::merge(ctx, space, args).await
        }
    }
}

/// The space a refusal names: the one `--space` gave, else this one.
fn named_space(args: &MergeArgs) -> String {
    match &args.space {
        Some(space) => format!("space {space}"),
        None => "this space".to_owned(),
    }
}
