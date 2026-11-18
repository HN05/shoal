//! `shoal merge`: merge a space's work, through its open PR on the forge or
//! a workspace's branch locally into the default branch, or a PR it names.
use anyhow::Result;

use crate::{
    cli::{
        args::merge::{MergeArgs, Merged},
        commands::items::select_item_target,
        context::Context,
        ui::{self, Fallback},
    },
    forge::items::reference::ItemInput,
    model::MergePath,
    protocol::{ItemContext, RepoTarget},
};

mod local;
mod pull;

pub(super) async fn run(ctx: &Context, args: MergeArgs) -> Result<i32> {
    match args.merged()? {
        Merged::Pr(item) => merge_named(ctx, &args, item).await,
        Merged::Work => merge_work(ctx, &args).await,
    }
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
    let target = select_item_target(ctx, space, repo, item.as_ref()).await?;
    pull::merge(ctx, target, item, args).await
}

/// Merge the work of the space `args` name as the daemon decides: through
/// its open PR, or a workspace's branch locally.
async fn merge_work(ctx: &Context, args: &MergeArgs) -> Result<i32> {
    let space = ui::select_space(ctx, args.space.clone(), Fallback::CurrentDirectory).await?;
    match crate::client::items::merge_path(&ctx.paths, space.clone()).await? {
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
            local::merge(ctx, space, args.push(), args.confirmation.yes).await
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
