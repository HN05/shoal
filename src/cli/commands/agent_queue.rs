//! `shoal add` launches under `max_agents`: the queued start workers and the
//! hold that keeps a starting launch's slot.
use std::time::Duration;

use anyhow::Result;
use tokio::io::AsyncReadExt as _;

use super::workspaces::AddPlan;
use crate::{
    agent::Agent,
    cli::{
        client::{self, request},
        context::Context,
        zmx,
    },
    model::{QueuedPlan, Workspace},
    paths::Paths,
    protocol::Method,
};

/// Reconnect a hold this soon after the daemon restarts.
const HOLD_RETRY: Duration = Duration::from_secs(1);

/// Record the workspace a starting entry opened, and label the zmx session it
/// runs in for `shoal attach`.
pub(super) async fn attach(
    ctx: &Context,
    entry: &str,
    workspace: &Workspace,
    agent: Option<&Agent>,
) -> Result<()> {
    request::<()>(
        &ctx.paths,
        Method::QueueAttach {
            entry: entry.to_owned(),
            workspace: workspace.id.clone(),
        },
    )
    .await?;
    if let Some(agent) = agent
        && let Err(error) = zmx::label_current(&workspace.id, &String::from(agent.clone())).await
    {
        eprintln!("warning: {error:#}");
    }
    Ok(())
}

/// Free a starting entry's slot after its launch failed; the daemon notifies
/// the user with `error`.
pub(super) async fn fail(ctx: &Context, entry: &str, error: &anyhow::Error) {
    let failed = request::<()>(
        &ctx.paths,
        Method::QueueFail {
            entry: entry.to_owned(),
            error: format!("{error:#}"),
        },
    )
    .await;
    if let Err(error) = failed {
        eprintln!("warning: cannot release the agent queue slot: {error:#}");
    }
}

/// Keeps a starting entry's slot while this process runs, until its agent
/// registers. It reconnects after a daemon restart.
pub(super) struct Hold(tokio::task::JoinHandle<()>);

impl Hold {
    pub fn start(paths: &Paths, entry: &str) -> Self {
        let (paths, entry) = (paths.clone(), entry.to_owned());
        Self(tokio::spawn(async move {
            loop {
                if let Ok((mut stream, body)) = client::open(
                    &paths,
                    Method::QueueHold {
                        entry: entry.clone(),
                    },
                )
                .await
                {
                    // A refusal means the agent took the slot over or the entry ended.
                    if body.into_result().is_err() {
                        return;
                    }
                    let mut closed = [0u8; 1];
                    let _ = stream.read(&mut closed).await;
                }
                tokio::time::sleep(HOLD_RETRY).await;
            }
        }))
    }
}

impl Drop for Hold {
    fn drop(&mut self) {
        self.0.abort();
    }
}

async fn plan(ctx: &Context, entry: &str) -> Result<(QueuedPlan, AddPlan)> {
    let queued = request::<QueuedPlan>(
        &ctx.paths,
        Method::QueuedPlan {
            entry: entry.to_owned(),
        },
    )
    .await?;
    let mut plan: AddPlan = serde_json::from_str(&queued.plan)?;
    plan.queue_entry = Some(entry.to_owned());
    Ok((queued, plan))
}

/// The daemon's worker for an admitted entry: open its tab or session.
pub(super) async fn start(ctx: &Context, entry: String) -> Result<i32> {
    let started = async {
        let (queued, plan) = plan(ctx, &entry).await?;
        super::workspaces::start_queued(ctx, plan, &queued.entry.name).await
    }
    .await;
    if let Err(error) = &started {
        fail(ctx, &entry, error).await;
    }
    started.map(|()| 0)
}

/// Add an admitted entry's workspace in this terminal and start its agent.
pub(super) async fn run(ctx: &Context, entry: String) -> Result<i32> {
    let (_, mut plan) = match plan(ctx, &entry).await {
        Ok(planned) => planned,
        Err(error) => {
            fail(ctx, &entry, &error).await;
            return Err(error);
        }
    };
    plan.detached = true;
    super::workspaces::execute_add(ctx, plan).await
}
