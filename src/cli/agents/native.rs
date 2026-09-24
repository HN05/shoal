//! Terminal agents run through the tracked execution wrapper.
use std::ffi::OsString;

use anyhow::{Context as _, Result};

use super::{
    open_app,
    trust::{trust_claude, trust_codex},
};
use crate::{
    cli::{
        CodexMode, client,
        context::Context,
        ui::{self, Fallback},
    },
    config::templates,
    execution,
    happy::HappyAgent,
    model::Workspace,
    protocol::ConfigTarget,
};

pub(in crate::cli) async fn claude(
    ctx: &Context,
    workspace: Option<String>,
    args: Vec<OsString>,
) -> Result<i32> {
    let workspace = ui::select_workspace(ctx, workspace, Fallback::CurrentDirectory).await?;
    let inspection = client::inspect(&ctx.paths, workspace).await?;
    let settings = client::settings(
        &ctx.paths,
        ConfigTarget::Workspace(inspection.workspace.id.clone()),
    )
    .await?;
    let instructions =
        templates::instructions(settings.agent_template.as_deref(), &inspection.workspace);
    trust_claude(ctx, &inspection.workspace.path);
    let args = templates::instruction_args(HappyAgent::Claude, instructions)
        .into_iter()
        .chain(args)
        .collect();
    let command = crate::config::named_commands::expand(
        &ctx.paths,
        &settings.commands,
        "claude",
        &inspection.workspace,
        args,
    )
    .await?;
    execution::run(
        &ctx.paths,
        inspection.workspace.id,
        command,
        Some("claude".into()),
    )
    .await
}

pub(in crate::cli) async fn codex(
    ctx: &Context,
    mode: Option<CodexMode>,
    workspace: Option<String>,
    args: Vec<OsString>,
) -> Result<i32> {
    let workspace = ui::select_workspace(ctx, workspace, Fallback::CurrentDirectory).await?;
    // Explicit desktop handoffs do not consume prompt templates.
    if mode == Some(CodexMode::App) {
        return open_app(ctx, Some(workspace), "codex", args).await;
    }
    let settings = client::settings(&ctx.paths, ConfigTarget::Workspace(workspace.clone())).await?;
    if mode.unwrap_or(settings.codex.default_mode) == CodexMode::App {
        return open_app(ctx, Some(workspace), "codex", args).await;
    }
    let inspection = client::inspect(&ctx.paths, workspace.clone()).await?;
    trust_codex(ctx, &inspection.workspace.path);
    let instructions =
        templates::instructions(settings.agent_template.as_deref(), &inspection.workspace);
    let args = templates::instruction_args(HappyAgent::Codex, instructions)
        .into_iter()
        .chain(args)
        .collect();
    let command = crate::config::named_commands::expand(
        &ctx.paths,
        &settings.commands,
        "codex",
        &inspection.workspace,
        args,
    )
    .await?;
    execution::run(&ctx.paths, workspace, command, Some("codex".into())).await
}

pub(super) async fn custom_agent(
    ctx: &Context,
    name: &str,
    workspace: Workspace,
    prompt: Option<String>,
    mut args: Vec<OsString>,
) -> Result<i32> {
    let settings =
        client::settings(&ctx.paths, ConfigTarget::Workspace(workspace.id.clone())).await?;
    let instructions = templates::instructions(settings.agent_template.as_deref(), &workspace);
    let prompt = [instructions, prompt.unwrap_or_default()]
        .into_iter()
        .filter(|part| !part.is_empty())
        .collect::<Vec<_>>()
        .join("\n\n");
    let argv = settings
        .commands
        .get(name)
        .with_context(|| format!("unknown agent {name:?}; define it in [commands]"))?;
    if !prompt.is_empty() && !argv.iter().any(|arg| arg.contains("{prompt}")) {
        args.insert(0, prompt.clone().into());
    }
    let command = crate::config::named_commands::expand_with_fields(
        &ctx.paths,
        &settings.commands,
        name,
        &workspace,
        args,
        &[("{prompt}", std::ffi::OsStr::new(&prompt))],
    )
    .await?;
    execution::run(&ctx.paths, workspace.id, command, Some(name.into())).await
}
