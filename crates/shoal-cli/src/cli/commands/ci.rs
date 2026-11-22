//! `shoal ci` and `shoal view ci`: list, start, read, rerun and cancel CI runs
//! of the workspace's repository, or of another one --repo or their URLs
//! name, and the checks on a PR's head.
mod act;
mod read;

use anyhow::Result;

pub(super) use self::read::{render_run, view};
use crate::{
    cli::{
        args::ci::CiCommand,
        commands::items::{current_context, select_item_target},
        context::Context,
        ui,
    },
    forge::{
        capability::Capability,
        ci::{CiAction, run_view::RunFilter},
        items::reference::ItemRef,
    },
    protocol::RepoTarget,
};
use shoal_forge::capability::CapabilityRequests;
use shoal_protocol::space_names::SpaceSelector;

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

/// The repository `ci run` starts a workflow in: as [`select_item_target`]
/// finds it, except that a workflow named by its URL, `url`, names its
/// repository and needs no context.
async fn workflow_target(
    ctx: &Context,
    space: Option<SpaceSelector>,
    repo: Option<String>,
    url: bool,
) -> Result<RepoTarget> {
    if space.is_none() && repo.is_none() && url && ui::current_chat(&space).is_none() {
        let context = current_context(ctx).await?;
        return Ok(RepoTarget {
            context,
            repository: None,
        });
    }
    select_item_target(
        ctx,
        space,
        repo,
        None,
        &crate::cli::commands::items::in_context(|target| {
            crate::client::ci::run(target, crate::forge::items::reference::ItemInput::Number(1))
        }),
    )
    .await
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
