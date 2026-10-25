//! `shoal forge ci`: start a CI workflow run on a branch of the workspace's
//! repository.
use anyhow::Result;

use crate::{
    cli::{RunArgs, client, context::Context, ui},
    forge::ci::run::{StartedRun, validate_run},
    protocol::Method,
};

pub(super) async fn run(ctx: &Context, args: RunArgs) -> Result<i32> {
    // Refuse bad arguments before the workspace picker asks for anything.
    validate_run(args.workflow.as_deref(), args.branch.as_deref())?;
    let workspace =
        ui::select_workspace(ctx, args.workspace, ui::Fallback::CurrentDirectory).await?;
    let started: StartedRun = client::request(
        &ctx.paths,
        Method::StartCiRun {
            workspace,
            workflow: args.workflow,
            branch: args.branch,
            inputs: args.inputs.into_iter().collect(),
        },
    )
    .await?;
    ctx.emit(&started_text(&started), &started)?;
    Ok(0)
}

/// The run that started and, where the forge reports one, its URL.
fn started_text(started: &StartedRun) -> String {
    let run = match &started.workflow {
        Some(workflow) => format!("{workflow} on {}", started.branch),
        None => started.branch.clone(),
    };
    match &started.url {
        Some(url) => format!("Run started for {run}: {url}"),
        None => format!("Run started for {run}"),
    }
}
