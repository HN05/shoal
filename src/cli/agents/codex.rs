//! Codex: its hooks deliver updates and report its turn state, its wrapper
//! wakes it by typing a prompt into its session, `config.toml` records the
//! directories it trusts, and `codex app` opens a workspace in the Codex
//! app.
use std::ffi::OsString;

use anyhow::Result;
use serde_json::Value;

use super::{
    AgentEnvironment, Integration, Wake,
    desktop::{self, App},
    trust::{CODEX, TrustFile},
};
use crate::{agent::hooks, ai::IntegrationKind, state::AgentState};

/// The Codex app, which `codex app` offers to install when it is missing.
pub(in crate::cli) const APP: App = App {
    name: "codex app",
    program: "codex",
    bundle: None,
    check: desktop::any_arguments,
    args: desktop::app_subcommand,
};

pub(super) struct Codex;

impl Integration for Codex {
    fn hook_args(&self, proxied: bool) -> std::io::Result<Vec<OsString>> {
        let mut args = config_overrides(true)?;
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

    fn chat_hook_args(&self) -> std::io::Result<Vec<OsString>> {
        config_overrides(false)
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

    fn app(&self) -> Option<&'static App> {
        Some(&APP)
    }
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
        ("Stop", hooks::turn_ended(shoal, IntegrationKind::Codex)),
    ]
}

mod layers;
mod provider;

use provider::PROVIDER;

#[cfg(test)]
mod tests;
