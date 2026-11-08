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

    /// Codex fires no hook after a failed turn, so the proxy's failure
    /// stream, which ends it, is the only word of it.
    fn proxy_ends_turns(&self) -> bool {
        true
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

    fn plan_change(&self, event: &Value) -> Option<crate::client::progress::ProgressChange> {
        plan::change(event)
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
/// Every agent reports its turn's state; a workspace's agent also gets the
/// hook that briefs its subagents about the workspace.
fn config_overrides(space: AgentSpace) -> std::io::Result<Vec<OsString>> {
    let shoal = hooks::shoal()?;
    let subagents = space.workspace().map(|_| hooks::subagent(&shoal));
    let groups = hooks::messages(&shoal)
        .chain(state_hooks(&shoal))
        .chain([hooks::plan(&shoal, IntegrationKind::Codex, plan::TOOLS)])
        .chain(subagents);
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

mod layers;
mod plan;
mod provider;

use provider::PROVIDER;

#[cfg(test)]
mod tests;
