//! Landing a workspace's branch: the operation that lands it, and the
//! tracked worker that merges it.
use std::path::PathBuf;

use anyhow::{Context as _, Result};

use crate::{
    cli::{
        client::{self, request},
        commands::{
            approvals,
            operations::{self, Reporter},
        },
        context::Context,
        ui::{self, Fallback},
    },
    daemon::approvals::Undecided,
    env,
    execution::{self, LandResult},
    model::{LandPlan, LandedBranch},
    protocol::{
        ConfigTarget, ExecutionMethod,
        operations::{OperationSpec, Step},
    },
};

/// `push` overrides `[land] push` when given.
pub(in crate::cli::commands) async fn land(
    ctx: &Context,
    workspace: Option<String>,
    push: Option<bool>,
) -> Result<i32> {
    let workspace = ui::select_workspace(ctx, workspace, Fallback::CurrentDirectory).await?;
    let spec = OperationSpec::Land { workspace, push };
    match operations::run(ctx, spec).await {
        Ok(landed) => {
            show_landed(ctx, &landed)?;
            Ok(0)
        }
        Err(error) => match error.downcast::<Undecided>() {
            Ok(Undecided(request)) => approvals::declined(ctx, &request),
            Err(error) => Err(error),
        },
    }
}

fn show_landed(ctx: &Context, landed: &LandedBranch) -> Result<()> {
    ctx.show(landed, |landed| {
        let refresh = &landed.default_refresh;
        if refresh.updated {
            outln!(
                "Updated {} from its upstream ({}..{})",
                refresh.branch,
                refresh.previous_commit,
                refresh.commit
            );
        }
        let (branch, default, commit) = (&landed.branch, &landed.default_branch, &landed.commit);
        if !landed.updated {
            outln!("{default} already contains {branch} ({commit})");
        } else if landed.fast_forward {
            outln!("Fast-forwarded {default} to {branch} ({commit})");
        } else {
            outln!("Merged {branch} into {default} ({commit})");
        }
        if let Some(remote) = &landed.pushed_to {
            outln!("Pushed {default} to {remote}");
        }
    })
}

/// Land `workspace` in an operation's worker, as a tracked execution whose
/// worker writes what it landed to a file; `push` overrides `[land] push`.
pub(in crate::cli::commands) async fn land_branch(
    ctx: &Context,
    workspace: String,
    push: Option<bool>,
    reporter: &Reporter<'_>,
) -> Result<LandedBranch> {
    let push = match push {
        Some(push) => push,
        None => {
            client::settings(&ctx.paths, ConfigTarget::Workspace(workspace.clone()))
                .await?
                .land
                .push
        }
    };
    reporter.step(Step::LandStarted).await?;
    let result = tempfile::NamedTempFile::new().context("create the landing result file")?;
    let code = execution::land(&ctx.paths, workspace, push, result.path().to_owned()).await?;
    let written = std::fs::read(result.path()).context("read the landing result")?;
    let landed: LandResult = match written.is_empty() {
        true => Err(format!("landing exited with status {code}")),
        false => serde_json::from_slice(&written).context("read the landing result")?,
    };
    landed.map_err(anyhow::Error::msg)
}

/// Merge the planned branch inside the landing execution, writing what it
/// landed, or why it could not, to `result`.
pub(in crate::cli::commands) async fn land_worker(
    ctx: &Context,
    plan: String,
    push: bool,
    result: PathBuf,
) -> Result<i32> {
    let landed: LandResult = merge(ctx, plan, push)
        .await
        .map_err(|error| format!("{error:#}"));
    std::fs::write(&result, serde_json::to_vec(&landed)?)
        .with_context(|| format!("write {}", result.display()))?;
    Ok(if landed.is_ok() { 0 } else { 1 })
}

async fn merge(ctx: &Context, plan: String, push: bool) -> Result<LandedBranch> {
    anyhow::ensure!(env::is_scoped(), "land worker requires a tracked execution");
    request::<()>(&ctx.paths, ExecutionMethod::CheckLanding).await?;
    let plan: LandPlan = serde_json::from_str(&plan)?;
    anyhow::ensure!(
        std::env::var(env::WORKSPACE_ID)? == plan.workspace.id,
        "land worker requires its authorized workspace"
    );
    crate::git::repo::finish_land(plan, push).await
}
