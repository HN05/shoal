//! `shoal message` queues a message for a workspace's agents; `shoal messages`
//! shows the workspace's undelivered messages once.
use anyhow::Result;

use crate::{
    cli::{
        client::request,
        context::Context,
        output::{Palette, Style},
        ui::{self, Fallback},
    },
    daemon::agent_messages::AgentMessage,
    protocol::Method,
};

pub(super) async fn send(ctx: &Context, workspace: Option<String>, message: String) -> Result<i32> {
    let workspace = ui::select_workspace(ctx, workspace, Fallback::CurrentDirectory).await?;
    request::<()>(&ctx.paths, Method::SendAgentMessage { workspace, message }).await?;
    ctx.emit("Message queued for the workspace's agents.", ())?;
    Ok(0)
}

pub(super) async fn show(ctx: &Context, workspace: Option<String>) -> Result<i32> {
    let workspace = ui::select_workspace(ctx, workspace, Fallback::CurrentDirectory).await?;
    let messages: Vec<AgentMessage> = request(
        &ctx.paths,
        Method::AgentMessages {
            workspace: workspace.clone(),
        },
    )
    .await?;
    ctx.show(&messages, |messages| {
        let palette = Palette::stdout(ctx.json);
        for message in messages {
            println!(
                "{}  {}",
                palette.paint(Style::Muted, crate::time::local_minutes(message.created_at)),
                message.message
            );
        }
        if messages.is_empty() {
            println!("No new messages");
        }
    })?;
    std::io::Write::flush(&mut std::io::stdout())?;
    delivered(ctx, workspace, &messages).await?;
    Ok(0)
}

/// Shown once: exactly the messages a caller printed do not come back.
pub(super) async fn delivered(
    ctx: &Context,
    workspace: String,
    messages: &[AgentMessage],
) -> Result<()> {
    if messages.is_empty() {
        return Ok(());
    }
    request::<()>(
        &ctx.paths,
        Method::MarkAgentMessagesDelivered {
            workspace,
            ids: messages.iter().map(|message| message.id).collect(),
        },
    )
    .await
}
