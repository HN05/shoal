//! The shortcut subcommands, such as `shoal codex` and `shoal t3`, which
//! start one agent or desktop app in a workspace, and whether each can.
use std::ffi::{OsStr, OsString};

use anyhow::Result;

use super::{LaunchOptions, desktop::App};
#[cfg(feature = "happy")]
use crate::happy::Agent;
use crate::{
    agent::AgentMode, ai::IntegrationKind, cli::context::Context, config::named_commands::Commands,
};

/// `shoal codex`, which runs the agent of integration `kind` or opens its
/// desktop app: `--cli` or `--app` overrides its `default_mode`.
pub(in crate::cli) async fn with_app(
    ctx: &Context,
    kind: IntegrationKind,
    mode: Option<AgentMode>,
    workspace: Option<String>,
    args: Vec<OsString>,
) -> Result<i32> {
    let integration = super::integration(kind);
    // Explicit desktop handoffs read no settings, so no prompt template.
    if let Some(app) = integration.app().filter(|_| mode == Some(AgentMode::App)) {
        let workspace = super::launch::shortcut_workspace(ctx, workspace).await?;
        let trust = integration.trust_file();
        return super::launch::open_app(ctx, &workspace, app, trust, args).await;
    }
    let options = LaunchOptions {
        tracked: mode == Some(AgentMode::Cli),
        ..Default::default()
    };
    let agent = kind.as_str().parse()?;
    super::launch::shortcut(ctx, agent, workspace, None, args, options).await
}

/// Whether `shoal codex` can start: the CLI as a tracked agent, or the Codex
/// app its `--app` opens.
fn codex_available(installed: &Installed) -> bool {
    installed.agent("codex") || app_available(&super::CODEX_APP, installed)
}

/// `shoal t3` and other shortcuts that only open an app.
pub(in crate::cli) async fn open_app(
    ctx: &Context,
    workspace: Option<String>,
    app: &App,
    args: Vec<OsString>,
) -> Result<i32> {
    let workspace = super::launch::shortcut_workspace(ctx, workspace).await?;
    super::launch::open_app(ctx, &workspace, app, None, args).await
}

/// Whether the desktop shortcut that opens `app` can start: it starts no
/// tracked agent, so it needs only the app.
fn app_available(app: &App, installed: &Installed) -> bool {
    app.available(installed.search_path)
}

/// `shoal happy <agent>`.
#[cfg(feature = "happy")]
pub(in crate::cli) async fn happy(
    ctx: &Context,
    agent: Agent,
    workspace: Option<String>,
    prompt: Option<String>,
    args: Vec<OsString>,
) -> Result<i32> {
    let name = format!("happy-{}", agent.as_str()).parse()?;
    let options = super::LaunchOptions::default();
    super::launch::shortcut(ctx, name, workspace, prompt, args, options).await
}

/// The agents `shoal happy <agent>` can start: each `happy-<agent>` offered
/// and installed.
#[cfg(feature = "happy")]
pub(in crate::cli) fn happy_agents<'a>(installed: &Installed<'a>) -> Vec<&'a str> {
    installed
        .agents
        .iter()
        .filter(|name| installed.agent(name))
        .filter_map(|name| name.strip_prefix("happy-"))
        .collect()
}

/// The agents offered under some settings, with their commands and the
/// search path, from which shortcuts judge whether they can start anything.
pub(in crate::cli) struct Installed<'a> {
    #[cfg_attr(not(feature = "happy"), allow(dead_code))]
    pub(in crate::cli) agents: &'a [String],
    pub(in crate::cli) commands: &'a Commands,
    pub(in crate::cli) search_path: &'a OsStr,
}

impl Installed<'_> {
    /// Whether agent `name` can launch.
    fn agent(&self, name: &str) -> bool {
        super::installed(name, self.commands, self.search_path)
    }
}

/// Each shortcut subcommand with whether it can start anything, so
/// completion offers only those that can. A tracked agent's shortcut needs
/// the agent; a desktop shortcut needs only the app.
pub(in crate::cli) fn shortcuts(installed: &Installed) -> Vec<(&'static str, bool)> {
    vec![
        ("claude", installed.agent("claude")),
        ("codex", codex_available(installed)),
        ("happy", !happy_agents(installed).is_empty()),
        ("t3", app_available(&super::desktop::T3, installed)),
    ]
}

/// The agents `shoal happy <agent>` can start.
#[cfg(not(feature = "happy"))]
pub(in crate::cli) fn happy_agents<'a>(_installed: &Installed<'a>) -> Vec<&'a str> {
    Vec::new()
}
