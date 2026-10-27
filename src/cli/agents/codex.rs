//! Codex: its hooks deliver updates and report its turn state, its wrapper
//! wakes it by typing a prompt into its session, `config.toml` records the
//! directories it trusts, and `codex.default_mode` may hand a launch to the
//! Codex app.
use std::ffi::OsString;

use anyhow::Result;
use serde_json::Value;

use super::{
    AgentEnvironment, Integration, Options, Wake,
    trust::{CODEX, TrustFile},
};
use crate::{
    agent::{CodexMode, hooks},
    config,
    state::AgentState,
};

/// The program that opens the Codex app.
pub(super) const APP: &str = "codex";

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
        url: &str,
        environment: &AgentEnvironment,
    ) -> Result<()> {
        if !args.iter().any(|arg| arg == PROVIDER) {
            return Ok(());
        }
        provider::route(args, url, &super::trust::codex_config(environment)?)
    }

    fn chat_hook_args(&self) -> std::io::Result<Vec<OsString>> {
        config_overrides(false)
    }

    fn routes_through_proxy(&self) -> bool {
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
}

/// What Codex's section sets: `codex.default_mode = "app"` hands launches to
/// the Codex app.
pub(super) fn options(config: &config::Codex) -> Options {
    Options {
        app: (config.default_mode == CodexMode::App).then_some(APP),
        ..Options::default()
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
        ("Stop", hooks::state(shoal, None, AgentState::Idle)),
    ]
}

mod provider;

use provider::PROVIDER;

#[cfg(test)]
mod tests;
