//! `shoal ci` and `shoal view ci`: list, start, read, rerun and cancel CI runs
//! of the workspace's repository, or of another one by their URLs, and the
//! checks on a PR's head.
mod act;
mod read;

use anyhow::Result;

pub(super) use self::read::{read_view, render_run, view};
use crate::{
    cli::{
        args::ci::CiCommand,
        commands::items::{current_context, select_item_context},
        context::Context,
    },
    forge::{
        ForgeRepo, IssueInput,
        capability::Capability,
        ci::{CiAction, run_view::RunFilter},
    },
    protocol::ItemContext,
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
            read::list(ctx, args.workspace, args.repo.name, args.pr, filter).await
        }
        CiCommand::Run(args) => act::start(ctx, args).await,
        CiCommand::Rerun { target, failed } => act::act(ctx, target, CiAction::Rerun, failed).await,
        CiCommand::Cancel { target } => act::act(ctx, target, CiAction::Cancel, false).await,
        CiCommand::Approve { subject } => act::approve(ctx, subject).await,
    }
}

/// Where a request about the run or workflow `input` names runs: the named
/// workspace or the registered repository `repo` names, else the current
/// workspace or checkout, which a run number or workflow name needs, so they
/// fall back to the picker; a URL names its repository and needs neither.
async fn input_context(
    ctx: &Context,
    workspace: Option<String>,
    repo: Option<String>,
    input: Option<&str>,
) -> Result<Option<ItemContext>> {
    let url = input.is_some_and(|input| IssueInput::parse(input) == IssueInput::Url);
    if workspace.is_none() && repo.is_none() && url {
        return current_context(ctx).await;
    }
    Ok(Some(select_item_context(ctx, workspace, repo, None).await?))
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
