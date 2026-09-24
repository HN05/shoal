//! Detached Happy sessions and their tool-specific prompt delivery.
use std::ffi::OsString;

use anyhow::{Context as _, Result};
use serde_json::json;

use super::trust::{trust_claude, trust_codex};
use crate::{
    cli::{
        client,
        context::Context,
        ui::{self, Fallback},
    },
    config::templates,
    execution,
    happy::{self, HappyAgent},
    protocol::ConfigTarget,
};

/// Start a Happy session the way Happy's own daemon does, so it registers with
/// that daemon and appears in the app, but detached from this terminal and
/// tracked like any other workspace command. The CLI returns once the launch
/// is recorded; the session's output goes to a log under Shoal's state.
pub(in crate::cli) async fn happy(
    ctx: &Context,
    agent: HappyAgent,
    workspace: Option<String>,
    prompt: Option<String>,
    mut args: Vec<OsString>,
) -> Result<i32> {
    let workspace = ui::select_workspace(ctx, workspace, Fallback::CurrentDirectory).await?;
    let workspace = client::inspect(&ctx.paths, workspace).await?.workspace;
    let settings =
        client::settings(&ctx.paths, ConfigTarget::Workspace(workspace.id.clone())).await?;
    let instructions = templates::instructions(settings.agent_template.as_deref(), &workspace);
    let prompt = if agent == HappyAgent::Codex && !instructions.is_empty() {
        Some(match prompt {
            Some(prompt) if !prompt.is_empty() => format!("{instructions}\n\n{prompt}"),
            _ => instructions,
        })
    } else {
        let mut configured = templates::instruction_args(HappyAgent::Claude, instructions);
        configured.append(&mut args);
        args = configured;
        prompt
    };
    match agent {
        HappyAgent::Claude => trust_claude(ctx, &workspace.path),
        HappyAgent::Codex => trust_codex(ctx, &workspace.path),
    }
    let daemon_state = happy::daemon_state_path(&ctx.paths.home);
    let daemon_recorded = daemon_state.is_file();
    if !daemon_recorded {
        eprintln!(
            "warning: Happy daemon state not found at {}; the session will not appear in the Happy app until `happy daemon start` runs",
            daemon_state.display()
        );
    }
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|elapsed| elapsed.as_millis())
        .unwrap_or_default();
    let state_dir = ctx.paths.workspace_state(&workspace.id);
    let stem = format!("happy-{}-{stamp}", agent.name());
    let log = state_dir.join(format!("{stem}.log"));
    // Happy's Codex mode has no prompt argument: the prompt is kept in a file
    // and, when this machine is logged in to Happy, delivered through Happy's
    // server to a session Shoal creates for the agent to attach to.
    let mut prompt_file = None;
    let mut seeded = None;
    let mut env = Vec::new();
    if let Some(prompt) = &prompt
        && !agent.accepts_prompt()
    {
        let path = state_dir.join(format!("{stem}.prompt.md"));
        std::fs::create_dir_all(&state_dir)
            .with_context(|| format!("create {}", state_dir.display()))?;
        std::fs::write(&path, prompt).with_context(|| format!("write {}", path.display()))?;
        match happy::client::seed(&ctx.paths, &workspace, agent).await {
            Ok(session) => {
                env = session.session.env();
                seeded = Some(session);
            }
            Err(error) => eprintln!(
                "warning: cannot deliver the prompt through Happy ({error:#}); send the prompt saved at {} to the session yourself",
                path.display()
            ),
        }
        prompt_file = Some(path);
    }
    let command = happy::command(agent, prompt.as_deref(), args);
    let launch = match execution::launch_detached(
        &ctx.paths,
        &workspace,
        log,
        command,
        &happy::client::RECONNECT_ENV,
        &env,
        Some(&format!("happy {}", agent.name())),
    )
    .await
    {
        Ok(launch) => launch,
        Err(error) => {
            // Nothing will ever attach to the seeded session; do not leave it in the app.
            if let Some(seeded) = seeded {
                seeded.discard().await;
            }
            return Err(error);
        }
    };
    // A prompt passed as an argument is delivered by the launch itself.
    let mut prompt_delivered = prompt.is_some() && agent.accepts_prompt();
    if let (Some(seeded), Some(prompt)) = (&seeded, &prompt) {
        match seeded
            .deliver(prompt, std::time::Duration::from_secs(90))
            .await
        {
            Ok(()) => prompt_delivered = true,
            Err(error) => eprintln!(
                "warning: prompt not delivered ({error:#}); send the prompt saved at {} to the session yourself",
                prompt_file
                    .as_deref()
                    .map(std::path::Path::display)
                    .unwrap()
            ),
        }
    }
    let delivery = match (&prompt_file, prompt_delivered) {
        (Some(_), true) => "\nPrompt delivered to the session",
        (Some(_), false) => "\nPrompt saved for you to send from the app",
        (None, _) => "",
    };
    ctx.emit(
        &format!(
            "Started Happy {} session in {} (execution {}, pid {})\nOutput: {}{delivery}",
            agent.name(),
            workspace.name,
            launch.execution_id,
            launch.pid,
            launch.log.display()
        ),
        json!({
            "workspace": workspace,
            "agent": agent,
            "execution_id": launch.execution_id,
            "pid": launch.pid,
            "log": launch.log,
            "prompt_file": prompt_file,
            "prompt_delivered": prompt_delivered,
            "happy_session_id": seeded.as_ref().map(|seeded| seeded.session.id.clone()),
            "happy_daemon_recorded": daemon_recorded,
        }),
    )?;
    Ok(0)
}
