//! `shoal ci` and `shoal view ci`: list, start, read, rerun and cancel CI runs
//! of the workspace's repository, and the checks on a PR's head.
mod act;
mod read;

use anyhow::Result;

pub(super) use self::read::{read_view, render_run, view};
use crate::{
    cli::{args::ci::CiCommand, context::Context},
    forge::{
        ForgeRepo,
        capability::Capability,
        ci::{CiAction, run_view::RunFilter},
    },
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
        CiCommand::Approve { subject } => act::approve(ctx, subject).await,
    }
}

/// What `view` and `status` say of the `waiting` runs on the head of the PR
/// at `pr_url` that wait for approval, with how to approve them.
pub(super) fn waiting_runs_line(pr_url: &str, waiting: usize) -> String {
    let (runs, them) = match waiting {
        1 => ("1 CI run waits".to_owned(), "it"),
        _ => (format!("{waiting} CI runs wait"), "them"),
    };
    let approvable = ForgeRepo::from_pull_url(pr_url)
        .ok()
        .filter(|forge| forge.supports(Capability::ApproveRuns))
        .and_then(|forge| Some(forge.pull(pr_url).ok()?.0));
    match approvable {
        Some(number) => {
            format!("{runs} for approval; approve {them} with shoal ci approve --pr {number}")
        }
        None => format!("{runs} for approval; approve {them} in the forge's web UI"),
    }
}
