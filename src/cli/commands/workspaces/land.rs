//! Landing a workspace's branch, and the tracked worker that merges it.
use anyhow::Result;

use crate::{
    cli::{
        client::{self, request},
        commands::approvals,
        context::Context,
        ui::{self, Fallback},
    },
    daemon::approvals::Undecided,
    env, execution,
    protocol::{ConfigTarget, ExecutionMethod},
};

/// `push` overrides `[land] push` when given.
pub(in crate::cli::commands) async fn land(
    ctx: &Context,
    workspace: Option<String>,
    push: Option<bool>,
) -> Result<i32> {
    let workspace = ui::select_workspace(ctx, workspace, Fallback::CurrentDirectory).await?;
    let push = match push {
        Some(push) => push,
        None => {
            client::settings(&ctx.paths, ConfigTarget::Workspace(workspace.clone()))
                .await?
                .land
                .push
        }
    };
    match execution::land(&ctx.paths, workspace, ctx.json, push).await {
        Err(error) => match error.downcast::<Undecided>() {
            Ok(Undecided(request)) => approvals::declined(ctx, &request),
            Err(error) => Err(error),
        },
        landed => landed,
    }
}

pub(in crate::cli::commands) async fn land_worker(
    ctx: &Context,
    plan: String,
    push: bool,
) -> Result<i32> {
    anyhow::ensure!(env::is_scoped(), "land worker requires a tracked execution");
    request::<()>(&ctx.paths, ExecutionMethod::CheckLanding).await?;
    let plan: crate::model::LandPlan = serde_json::from_str(&plan)?;
    anyhow::ensure!(
        std::env::var(env::WORKSPACE_ID)? == plan.workspace.id,
        "land worker requires its authorized workspace"
    );
    let result = crate::git::repo::finish_land(plan, push).await?;
    ctx.show(&result, |result| {
        let refresh = &result.default_refresh;
        if refresh.updated {
            outln!(
                "Updated {} from its upstream ({}..{})",
                refresh.branch,
                refresh.previous_commit,
                refresh.commit
            );
        }
        let (branch, default, commit) = (&result.branch, &result.default_branch, &result.commit);
        if !result.updated {
            outln!("{default} already contains {branch} ({commit})");
        } else if result.fast_forward {
            outln!("Fast-forwarded {default} to {branch} ({commit})");
        } else {
            outln!("Merged {branch} into {default} ({commit})");
        }
        if let Some(remote) = &result.pushed_to {
            outln!("Pushed {default} to {remote}");
        }
    })?;
    Ok(0)
}
