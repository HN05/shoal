//! Codex: its hooks deliver updates and report its turn state, its wrapper
//! wakes it by typing a prompt into its session, `config.toml` records the
//! directories it trusts, and `codex.default_mode` may hand a launch to the
//! Codex app.
use std::ffi::OsString;

use anyhow::Result;
use futures_util::future::BoxFuture;
use serde_json::Value;

use super::{
    Integration, Launch, desktop,
    trust::{CODEX, TrustFile},
};
use crate::{
    agent::{CodexMode, hooks},
    cli::{
        client,
        context::Context,
        terminal,
        ui::{self, Fallback},
    },
    config::{Effective, templates::Delivery},
    protocol::ConfigTarget,
    state::AgentState,
};

pub(super) struct Codex;

impl Integration for Codex {
    fn hook_args(&self) -> std::io::Result<Vec<OsString>> {
        config_overrides(true)
    }

    fn chat_hook_args(&self) -> std::io::Result<Vec<OsString>> {
        config_overrides(false)
    }

    /// Its wrapper starts an idle Codex's next turn only in a session it can
    /// type into.
    fn delivery(&self, ctx: &Context, settings: &Effective) -> Delivery {
        if terminal::agents_can_be_prompted(ctx, settings) {
            Delivery::Pushed
        } else {
            Delivery::Watched
        }
    }

    fn trust_file(&self) -> Option<&'static TrustFile> {
        Some(&CODEX)
    }

    fn wakes_by_prompt(&self) -> bool {
        true
    }

    /// `codex.default_mode = "app"` opens the Codex app, which cannot receive
    /// a prompt, so a launch with one stays in the terminal, as a tracked one
    /// does.
    fn launches_itself(&self, settings: &Effective, prompted: bool, tracked: bool) -> bool {
        !prompted && !tracked && settings.codex.default_mode == CodexMode::App
    }

    fn launch<'a>(
        &'a self,
        ctx: &'a Context,
        launch: &'a mut Launch,
    ) -> BoxFuture<'a, Result<Option<i32>>> {
        Box::pin(async move {
            if !self.launches_itself(&launch.settings, launch.prompt.is_some(), launch.tracked) {
                return Ok(None);
            }
            let args = std::mem::take(&mut launch.args);
            desktop::open_app(ctx, &launch.workspace, "codex", Some(&CODEX), args)
                .await
                .map(Some)
        })
    }
}

/// `shoal codex`: `--cli` or `--app` overrides `codex.default_mode`.
pub(in crate::cli) async fn shortcut(
    ctx: &Context,
    mode: Option<CodexMode>,
    workspace: Option<String>,
    args: Vec<OsString>,
) -> Result<i32> {
    let workspace = ui::select_workspace(ctx, workspace, Fallback::CurrentDirectory).await?;
    let workspace = client::inspect(&ctx.paths, workspace).await?.workspace;
    // Explicit desktop handoffs do not consume prompt templates.
    if mode == Some(CodexMode::App) {
        return desktop::open_app(ctx, &workspace, "codex", Some(&CODEX), args).await;
    }
    let settings =
        client::settings(&ctx.paths, ConfigTarget::Workspace(workspace.id.clone())).await?;
    let mut launch = Launch {
        name: "codex".into(),
        workspace,
        settings,
        prompt: None,
        args,
        tracked: false,
        replaces: Vec::new(),
    };
    if mode.is_none()
        && let Some(code) = Codex.launch(ctx, &mut launch).await?
    {
        return Ok(code);
    }
    super::launch::tracked(ctx, &Codex, launch).await
}

/// Whether `shoal codex` can start: the CLI as a tracked agent, or the Codex
/// app its `--app` opens.
pub(super) fn shortcut_available(installed: &super::Installed) -> bool {
    installed.agent("codex") || desktop::available("codex", installed)
}

/// Codex takes each event's groups as a `-c hooks.<event>=` TOML value.
/// `turn_state` adds the hooks of a workspace's agent: those that report the
/// turn's state and the one that briefs its subagents.
fn config_overrides(turn_state: bool) -> std::io::Result<Vec<OsString>> {
    let shoal = hooks::shoal()?;
    let state = if turn_state {
        let mut state = state_hooks(&shoal);
        state.push(hooks::subagent(&shoal));
        state
    } else {
        Vec::new()
    };
    Ok(hooks::by_event(hooks::messages(&shoal).chain(state))
        .into_iter()
        .flat_map(|(event, groups)| {
            let groups: toml::Value =
                serde_json::from_value(groups).expect("hook groups are valid TOML");
            ["-c".into(), format!("hooks.{event}={groups}").into()]
        })
        .collect())
}

/// Each Codex hook event with the group that reports its state. A session
/// that starts, resumes or clears waits for its first turn. Codex has no
/// notice that its prompt waits for input, so an interrupted turn stays
/// working until its next turn ends.
fn state_hooks(shoal: &str) -> Vec<(&'static str, Value)> {
    vec![
        (
            "SessionStart",
            hooks::state(shoal, Some(hooks::SESSION_START), AgentState::Idle),
        ),
        (
            "UserPromptSubmit",
            hooks::state(shoal, None, AgentState::Working),
        ),
        (
            "PostToolUse",
            hooks::state(shoal, None, AgentState::Working),
        ),
        ("Stop", hooks::state(shoal, None, AgentState::Idle)),
    ]
}

#[cfg(test)]
mod tests;
