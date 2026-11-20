//! Codex: its hooks deliver updates and report its turn state, its wrapper
//! wakes it by typing a prompt into its session, and `config.toml` records
//! the directories it trusts.
use std::ffi::OsString;

use anyhow::Result;
use serde_json::Value;

use super::{
    AgentEnvironment, AgentSpace, Integration, Wake, hooks,
    trust::{CODEX, TrustFile},
};
use crate::{ai::IntegrationKind, state::AgentState};

pub(super) struct Codex;

impl Integration for Codex {
    fn hook_args(&self, space: AgentSpace, proxied: bool) -> std::io::Result<Vec<OsString>> {
        let mut args = shoal_overrides(space)?;
        if proxied {
            args.push(PROVIDER.into());
        }
        Ok(args)
    }

    /// Each server as one `-c mcp_servers.<name>=` TOML table, which
    /// replaces a server of the same name in Codex's own config whole, so
    /// none of its fields, such as `enabled`, stays.
    fn mcp_args(&self, servers: &[(&str, &crate::config::mcp::Server)]) -> Vec<OsString> {
        (servers.iter())
            .flat_map(|(name, server)| {
                let env: toml::Table = (server.env.iter())
                    .map(|(variable, value)| (variable.clone(), value.as_str().into()))
                    .collect();
                let table = toml::Table::from_iter([
                    ("command".to_owned(), server.command.as_str().into()),
                    ("args".to_owned(), server.args.clone().into()),
                    ("env".to_owned(), toml::Value::Table(env)),
                ]);
                let table = toml::Value::Table(table);
                ["-c".into(), format!("mcp_servers.{name}={table}").into()]
            })
            .collect()
    }

    /// The provider placeholder becomes the overrides that send the
    /// provider the launch uses to the proxy.
    fn route_through_proxy(
        &self,
        args: &mut Vec<OsString>,
        routing: &super::Routing,
        environment: &AgentEnvironment,
    ) -> Result<()> {
        if !args.iter().any(|arg| arg == PROVIDER) {
            return Ok(());
        }
        let config = super::trust::codex_config(environment)?;
        let codex_home = config.parent().unwrap_or(routing.directory);
        provider::route(args, routing.url, codex_home, routing.directory)
    }

    /// The base URL of the provider the launch uses.
    fn default_upstream(
        &self,
        args: &[OsString],
        directory: &std::path::Path,
        variable: &dyn Fn(&str) -> Option<OsString>,
        home: &std::path::Path,
    ) -> Result<String> {
        let codex_home = super::trust::codex_home(variable, home)?;
        provider::base_url(args, &codex_home, directory, variable)
    }

    /// Codex fires no hook after a failed turn, so the proxy's failure
    /// stream, which ends it, is the only word of it.
    fn proxy_ends_turns(&self) -> bool {
        true
    }

    /// Codex's base URL takes the OpenAI API's version, as each gateway
    /// serves it.
    fn api_path(&self) -> &'static str {
        "/v1"
    }

    fn proxy_args(&self) -> Vec<OsString> {
        vec![PROVIDER.into()]
    }

    fn trust_file(&self) -> Option<&'static TrustFile> {
        Some(&CODEX)
    }

    fn wakes(&self) -> Wake {
        Wake::Prompt
    }

    fn subagent_change(&self, event: &Value) -> Option<crate::protocol::SubagentChange> {
        hooks::subagent_change(event)
    }

    fn plan_change(&self, event: &Value) -> Option<shoal_protocol::progress::ProgressChange> {
        plan::change(event)
    }

    fn tool_change(&self, event: &Value) -> Option<shoal_protocol::changes::ToolChange> {
        changes::change(event)
    }

    /// Ctrl-C, which quits Codex only when pressed again.
    fn interrupt_key(&self) -> Option<&'static str> {
        Some("\x03")
    }
}

/// The setting that lets Codex's workspace-write sandbox, which blocks the
/// network, reach Shoal's daemon socket; Codex allows a single socket only
/// along with the whole network. Git pushes need the network too.
const DAEMON_ACCESS: [&str; 2] = ["-c", "sandbox_workspace_write.network_access=true"];

/// What every Codex launch takes from Shoal: its hooks, see
/// [`config_overrides`], and access to the daemon from Codex's sandbox.
fn shoal_overrides(space: AgentSpace) -> std::io::Result<Vec<OsString>> {
    let mut args = config_overrides(space)?;
    args.extend(DAEMON_ACCESS.map(OsString::from));
    Ok(args)
}

/// Codex takes each event's groups as a `-c hooks.<event>=` TOML value.
/// Every agent reports its turn's state and its subagents; a workspace's
/// agent also gets the hook that briefs its subagents about the workspace,
/// and those that report what its tool calls change.
fn config_overrides(space: AgentSpace) -> std::io::Result<Vec<OsString>> {
    let shoal = hooks::shoal()?;
    let subagents = space.workspace().map(|_| hooks::subagent(&shoal));
    // Codex's matchers name its tools as it chooses; the report picks the
    // calls that change files.
    let changes = (space.workspace().into_iter()).flat_map(|_| {
        ["PreToolUse", "PostToolUse"]
            .map(|event| (event, hooks::changes(&shoal, IntegrationKind::Codex, None)))
    });
    let groups = hooks::messages(&shoal)
        .chain(state_hooks(&shoal))
        .chain([hooks::plan(&shoal, IntegrationKind::Codex, plan::TOOLS)])
        .chain(hooks::subagent_reports(&shoal, IntegrationKind::Codex))
        .chain(subagents)
        .chain(changes);
    Ok(hooks::by_event(groups)
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
        hooks::session_start(shoal),
        (
            "UserPromptSubmit",
            hooks::state(shoal, None, AgentState::Working),
        ),
        (
            "PostToolUse",
            hooks::state(shoal, None, AgentState::Working),
        ),
        ("Stop", hooks::turn_ended(shoal, IntegrationKind::Codex)),
    ]
}

mod changes;
mod layers;
mod plan;
mod provider;

use provider::PROVIDER;

#[cfg(test)]
mod tests;
