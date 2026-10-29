//! `shoal ci` and `shoal view ci`: list, start, read, rerun and cancel CI runs
//! of the workspace's repository, and the checks on a PR's head.
mod act;
mod read;

use anyhow::Result;

pub(super) use self::read::view;
use crate::{
    cli::{args::ci::CiCommand, context::Context},
    forge::ci::{CiAction, run_view::RunFilter},
};

pub(super) async fn run(ctx: &Context, command: CiCommand) -> Result<i32> {
    match command {
        CiCommand::List(args) => {
            let filter = RunFilter {
                branch: args.branch,
                workflow: args.workflow,
                commit: None,
                limit: args.limit,
            };
            read::list(ctx, args.workspace, args.pr, filter).await
        }
        CiCommand::Run(args) => act::start(ctx, args).await,
        CiCommand::Rerun { target, failed } => act::act(ctx, target, CiAction::Rerun, failed).await,
        CiCommand::Cancel { target } => act::act(ctx, target, CiAction::Cancel, false).await,
    }
}
