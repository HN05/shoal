//! Terminal agents run through the tracked execution wrapper.
use std::ffi::{OsStr, OsString};

use anyhow::{Context as _, Result};

use super::{
    open_app,
    trust::{trust_claude, trust_codex},
};
use crate::{
    agent::{BuiltinAgent, CodexMode},
    cli::{
        client,
        context::Context,
        ui::{self, Fallback},
    },
    config::{Effective, named_commands, templates},
    execution,
    model::Workspace,
    protocol::ConfigTarget,
};

pub(in crate::cli) async fn claude(
    ctx: &Context,
    workspace: Option<String>,
    args: Vec<OsString>,
) -> Result<i32> {
    let workspace = ui::select_workspace(ctx, workspace, Fallback::CurrentDirectory).await?;
    let launch = ResolvedLaunch::inspect(ctx, workspace, None).await?;
    trust_claude(ctx, &launch.workspace.path);
    let args = templates::instruction_args(BuiltinAgent::Claude, launch.instructions())
        .into_iter()
        .chain(args)
        .collect();
    launch.run(ctx, "claude", args, "").await
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
    let launch = ResolvedLaunch::inspect(ctx, workspace, Some(settings)).await?;
    trust_codex(ctx, &launch.workspace.path);
    let args = templates::instruction_args(BuiltinAgent::Codex, launch.instructions())
        .into_iter()
        .chain(args)
        .collect();
    launch.run(ctx, "codex", args, "").await
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
    let launch = ResolvedLaunch {
        workspace,
        settings,
    };
    let prompt = [launch.instructions(), prompt.unwrap_or_default()]
        .into_iter()
        .filter(|part| !part.is_empty())
        .collect::<Vec<_>>()
        .join("\n\n");
    let argv = launch
        .settings
        .commands
        .get(name)
        .with_context(|| format!("unknown agent {name:?}; define it in [commands]"))?;
    if !prompt.is_empty() && !argv.iter().any(|arg| arg.contains("{prompt}")) {
        args.insert(0, prompt.clone().into());
    }
    launch.run(ctx, name, args, &prompt).await
}

/// Resolved for one terminal launch; settings are never cached across launches.
struct ResolvedLaunch {
    workspace: Workspace,
    settings: Effective,
}

impl ResolvedLaunch {
    /// Codex supplies the settings it already read to choose CLI or desktop mode.
    async fn inspect(
        ctx: &Context,
        workspace: String,
        settings: Option<Effective>,
    ) -> Result<Self> {
        let workspace = client::inspect(&ctx.paths, workspace).await?.workspace;
        let settings = match settings {
            Some(settings) => settings,
            None => {
                client::settings(&ctx.paths, ConfigTarget::Workspace(workspace.id.clone())).await?
            }
        };
        Ok(Self {
            workspace,
            settings,
        })
    }

    fn instructions(&self) -> String {
        templates::instructions(self.settings.agent_template.as_deref(), &self.workspace)
    }

    async fn run(
        self,
        ctx: &Context,
        name: &str,
        args: Vec<OsString>,
        prompt: &str,
    ) -> Result<i32> {
        let command = named_commands::expand_with_fields(
            &ctx.paths,
            &self.settings.commands,
            name,
            &self.workspace,
            args,
            &[("{prompt}", OsStr::new(prompt))],
        )
        .await?;
        execution::run(&ctx.paths, self.workspace.id, command, Some(name.into())).await
    }
}
