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

/// Agent hooks run after every tool call, so a hook prints nothing without
/// messages and reports failures without blocking the agent.
pub(super) async fn hook(ctx: &Context) -> Result<i32> {
    let mut input = String::new();
    // Hook input is optional context; a terminal, closed or unreadable stdin
    // still delivers.
    if !std::io::IsTerminal::is_terminal(&std::io::stdin()) {
        let _ = std::io::Read::read_to_string(&mut std::io::stdin(), &mut input);
    }
    if let Err(error) = deliver_to_hook(ctx, &input).await {
        eprintln!("shoal: agent messages unavailable: {error:#}");
    }
    Ok(0)
}

async fn deliver_to_hook(ctx: &Context, input: &str) -> Result<()> {
    let workspace = ui::select_workspace(ctx, None, Fallback::CurrentDirectoryOnly).await?;
    let messages: Vec<AgentMessage> = request(
        &ctx.paths,
        Method::AgentMessages {
            workspace: workspace.clone(),
        },
    )
    .await?;
    if messages.is_empty() {
        return Ok(());
    }
    println!("{}", hook_output(input, &messages));
    std::io::Write::flush(&mut std::io::stdout())?;
    delivered(ctx, workspace, &messages).await
}

/// Claude Code and Codex read `additionalContext` for the event named in the
/// hook input; without one, the context is plain text.
fn hook_output(input: &str, messages: &[AgentMessage]) -> String {
    let context = std::iter::once("New Shoal messages for this workspace:".to_owned())
        .chain(
            messages
                .iter()
                .map(|message| format!("- {}", message.message)),
        )
        .collect::<Vec<_>>()
        .join("\n");
    let event = serde_json::from_str::<serde_json::Value>(input)
        .ok()
        .and_then(|input| input["hook_event_name"].as_str().map(str::to_owned));
    match event {
        Some(event) => serde_json::json!({
            "hookSpecificOutput": {"hookEventName": event, "additionalContext": context}
        })
        .to_string(),
        None => context,
    }
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hook_output_names_the_event_it_answers() {
        let messages = [AgentMessage {
            id: 1,
            created_at: 0,
            message: "Stop the dev server".into(),
        }];
        let output: serde_json::Value = serde_json::from_str(&hook_output(
            r#"{"hook_event_name":"PostToolUse","tool_name":"Bash"}"#,
            &messages,
        ))
        .unwrap();
        assert_eq!(output["hookSpecificOutput"]["hookEventName"], "PostToolUse");
        assert_eq!(
            output["hookSpecificOutput"]["additionalContext"],
            "New Shoal messages for this workspace:\n- Stop the dev server"
        );
        assert_eq!(
            hook_output("", &messages),
            "New Shoal messages for this workspace:\n- Stop the dev server"
        );
    }
}
