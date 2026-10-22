//! Terminal agents run through the tracked execution wrapper.
use std::{
    ffi::{OsStr, OsString},
    path::PathBuf,
};

use anyhow::Result;

use super::{
    ensure_installed, open_app,
    trust::{trust_claude, trust_codex},
};
use crate::{
    agent::{Agent, BuiltinAgent, CodexMode},
    cli::{
        client,
        context::Context,
        terminal,
        ui::{self, Fallback},
    },
    config::{
        Effective, named_commands,
        templates::{self, Delivery},
    },
    execution,
    model::Workspace,
    protocol::ConfigTarget,
};

/// `replaces` names recovery records of stopped agents this one replaces,
/// which its wrapper consumes once it registers.
pub(in crate::cli) async fn claude(
    ctx: &Context,
    workspace: Option<String>,
    args: Vec<OsString>,
    replaces: Vec<PathBuf>,
) -> Result<i32> {
    let workspace = ui::select_workspace(ctx, workspace, Fallback::CurrentDirectory).await?;
    let launch = ResolvedLaunch::inspect(ctx, workspace, None).await?;
    ensure_installed(&Agent::Claude, &launch.settings.commands)?;
    trust_claude(ctx, &launch.workspace).await;
    // Claude Code's hooks deliver updates and start its next turn with them.
    let instructions = launch.instructions(Delivery::Pushed);
    let args = templates::instruction_args(BuiltinAgent::Claude, instructions)
        .into_iter()
        .chain(BuiltinAgent::Claude.hook_args()?)
        .chain(args)
        .collect();
    launch.run(ctx, "claude", args, "", replaces).await
}

/// `replaces` as for [`claude`]; a desktop handoff consumes them once it opens.
pub(in crate::cli) async fn codex(
    ctx: &Context,
    mode: Option<CodexMode>,
    workspace: Option<String>,
    args: Vec<OsString>,
    replaces: Vec<PathBuf>,
) -> Result<i32> {
    let workspace = ui::select_workspace(ctx, workspace, Fallback::CurrentDirectory).await?;
    // Explicit desktop handoffs do not consume prompt templates.
    if mode == Some(CodexMode::App) {
        return started(
            open_app(ctx, Some(workspace), "codex", args).await,
            &replaces,
        );
    }
    let settings = client::settings(&ctx.paths, ConfigTarget::Workspace(workspace.clone())).await?;
    if mode.unwrap_or(settings.codex.default_mode) == CodexMode::App {
        return started(
            open_app(ctx, Some(workspace), "codex", args).await,
            &replaces,
        );
    }
    let launch = ResolvedLaunch::inspect(ctx, workspace, Some(settings)).await?;
    ensure_installed(&Agent::Codex, &launch.settings.commands)?;
    trust_codex(ctx, &launch.workspace).await;
    // Its wrapper starts an idle Codex's next turn only in a zmx session.
    let delivery = if terminal::agents_can_be_prompted(&launch.settings) {
        Delivery::Pushed
    } else {
        Delivery::Watched
    };
    let args = templates::instruction_args(BuiltinAgent::Codex, launch.instructions(delivery))
        .into_iter()
        .chain(BuiltinAgent::Codex.hook_args()?)
        .chain(args)
        .collect();
    launch.run(ctx, "codex", args, "", replaces).await
}

pub(super) async fn custom_agent(
    ctx: &Context,
    name: &str,
    workspace: Workspace,
    prompt: Option<String>,
    mut args: Vec<OsString>,
    replaces: Vec<PathBuf>,
) -> Result<i32> {
    let settings =
        client::settings(&ctx.paths, ConfigTarget::Workspace(workspace.id.clone())).await?;
    let launch = ResolvedLaunch {
        workspace,
        settings,
    };
    let prompt = [
        launch.instructions(Delivery::Watched),
        prompt.unwrap_or_default(),
    ]
    .into_iter()
    .filter(|part| !part.is_empty())
    .collect::<Vec<_>>()
    .join("\n\n");
    let commands = &launch.settings.commands;
    ensure_installed(&Agent::Custom(name.into()), commands)?;
    let argv = &commands[name];
    if !prompt.is_empty() && !argv.iter().any(|arg| arg.contains("{prompt}")) {
        args.insert(0, prompt.clone().into());
    }
    launch.run(ctx, name, args, &prompt, replaces).await
}

/// The result of a launch that returns once its agent has started, after
/// consuming the recovery records it replaces.
pub(super) fn started(launched: Result<i32>, replaces: &[PathBuf]) -> Result<i32> {
    let code = launched?;
    for record in replaces {
        crate::execution::recovery::consume(record)?;
    }
    Ok(code)
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

    fn instructions(&self, delivery: Delivery) -> String {
        templates::instructions(
            self.settings.agent_template.as_deref(),
            &self.workspace,
            delivery,
        )
    }

    async fn run(
        self,
        ctx: &Context,
        name: &str,
        args: Vec<OsString>,
        prompt: &str,
        replaces: Vec<PathBuf>,
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
        if let Some(code) = terminal::run_agent(
            ctx,
            &self.settings,
            &self.workspace,
            name,
            &replaces,
            &command,
        )
        .await?
        {
            return Ok(code);
        }
        if replaces.is_empty() {
            return execution::run(&ctx.paths, self.workspace.id, command, Some(name.into())).await;
        }
        execution::run_recovery(
            &ctx.paths,
            self.workspace.id,
            command,
            name.into(),
            replaces,
        )
        .await
    }
}
