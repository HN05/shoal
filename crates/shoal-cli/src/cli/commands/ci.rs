//! `shoal ci` and `shoal view ci`: list, start, read, rerun and cancel CI runs
//! of the workspace's repository, or of another one by their URLs, and the
//! checks on a PR's head.
mod act;
mod read;

use anyhow::Result;

pub(super) use self::read::{render_run, view};
use crate::client::space_names::SpaceSelector;
use crate::{
    cli::{
        args::ci::CiCommand,
        commands::items::{current_context, select_space_item_context},
        context::Context,
        ui,
    },
    forge::{
        capability::Capability,
        ci::{CiAction, run_view::RunFilter},
        items::reference::ItemRef,
    },
    protocol::ItemContext,
};

pub(super) async fn run(ctx: &Context, command: CiCommand) -> Result<i32> {
    match command {
        CiCommand::Ls(args) => {
            let filter = RunFilter {
                branch: args.branch,
                workflow: args.workflow,
                commit: None,
                limit: args.limit,
            };
            read::list(ctx, args.space, args.repo.name, args.pr, filter).await
        }
        CiCommand::Run(args) => act::start(ctx, args).await,
        CiCommand::Rerun { target, failed } => act::act(ctx, target, CiAction::Rerun, failed).await,
        CiCommand::Cancel { target } => act::act(ctx, target, CiAction::Cancel, false).await,
        CiCommand::Approve { subject } => act::approve(ctx, subject).await,
    }
}

/// Where a request about a run or workflow runs: the named workspace or the
/// registered repository `repo` names, else the current workspace or
/// checkout, which a run number or workflow name needs, so they fall back to
/// the picker; a run or workflow named by its URL, `url`, names its
/// repository and needs neither.
async fn input_context(
    ctx: &Context,
    space: Option<SpaceSelector>,
    repo: Option<String>,
    url: bool,
) -> Result<Option<ItemContext>> {
    if space.is_none() && repo.is_none() && url && ui::current_chat(&space).is_none() {
        return current_context(ctx).await;
    }
    Ok(Some(
        select_space_item_context(ctx, space, repo, None).await?,
    ))
}

/// What `view` and `status` say of the `waiting` runs on the head of the PR
/// `pull` that wait for approval, with how to approve them.
pub(super) fn waiting_runs_line(pull: &ItemRef, waiting: usize) -> String {
    let (runs, them) = match waiting {
        1 => ("1 CI run waits".to_owned(), "it"),
        _ => (format!("{waiting} CI runs wait"), "them"),
    };
    let approvable = (pull.repository().supports(Capability::ApproveRuns))
        .then(|| pull.pull().ok())
        .flatten();
    match approvable {
        Some(number) => {
            format!("{runs} for approval; approve {them} with shoal ci approve --pr {number}")
        }
        None => format!("{runs} for approval; approve {them} in the forge's web UI"),
    }
}
