//! `shoal internal agent-state`: the turn state an agent's hooks report for its workspace.
use anyhow::Result;

use crate::{
    cli::{
        client::request,
        context::Context,
        ui::{self, Fallback},
    },
    model::AgentStatus,
    protocol::Method,
    state::AgentState,
};

pub(super) async fn run(
    ctx: &Context,
    workspace: Option<String>,
    state: AgentState,
) -> Result<i32> {
    let workspace = ui::select_workspace(ctx, workspace, Fallback::CurrentDirectory).await?;
    let status: AgentStatus =
        request(&ctx.paths, Method::SetAgentState { workspace, state }).await?;
    ctx.emit(
        &format!("Agent state: {}", ui::agent_state_label(status.state)),
        status,
    )?;
    Ok(0)
}
