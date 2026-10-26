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
    /// provider Codex uses to the proxy.
    fn route_through_proxy(
        &self,
        args: &mut Vec<OsString>,
        url: &str,
        environment: &AgentEnvironment,
    ) -> Result<()> {
        let Some(at) = args.iter().position(|arg| arg == PROVIDER) else {
            return Ok(());
        };
        let config = super::trust::codex_config(environment)?;
        let overrides = proxy_overrides(active_provider(&config)?.as_deref(), url)?;
        args.splice(at..=at, overrides);
        Ok(())
    }

    fn chat_hook_args(&self) -> std::io::Result<Vec<OsString>> {
        config_overrides(false)
    }

    fn routes_through_proxy(&self) -> bool {
        true
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

/// Where a proxied Codex launch's hook arguments name its provider, which
/// [`Codex::route_through_proxy`] turns into overrides once the agent's
/// environment says which provider its config uses.
const PROVIDER: &str = "{shoal-proxy-codex-provider}";

/// Codex's built-in providers, which its config cannot override.
const BUILT_IN_PROVIDERS: [&str; 4] = ["openai", "oss", "ollama", "lmstudio"];

/// The provider Shoal defines for a Codex on its built-in OpenAI provider,
/// which uses the same login.
const PROXY_PROVIDER: &str = "shoal-proxy";

/// The overrides that send Codex's `provider`, `openai` when its config
/// names none, to the proxy at `url`. A provider of the user's keeps its own
/// settings, such as a token, and only changes its base URL; the built-in
/// one, which Codex does not let config change, gives way to Shoal's provider
/// with the same OpenAI login. The proxy forwards plain HTTP.
fn proxy_overrides(provider: Option<&str>, url: &str) -> Result<Vec<OsString>> {
    let url = toml::Value::String(url.to_owned());
    let overrides = match provider.unwrap_or("openai") {
        "openai" => vec![
            format!("model_provider={PROXY_PROVIDER}"),
            format!(
                "model_providers.{PROXY_PROVIDER}={{name=\"Shoal proxy\",base_url={url},\
                 wire_api=\"responses\",requires_openai_auth=true,supports_websockets=false}}"
            ),
        ],
        id if BUILT_IN_PROVIDERS.contains(&id) => {
            anyhow::bail!("Codex's built-in provider {id:?} cannot go through Shoal's proxy")
        }
        id => {
            let key = toml_edit::Key::new(id);
            vec![
                format!("model_providers.{key}.base_url={url}"),
                format!("model_providers.{key}.supports_websockets=false"),
            ]
        }
    };
    Ok(overrides
        .into_iter()
        .flat_map(|value| ["-c".into(), value.into()])
        .collect())
}

/// The provider `config` names with `model_provider`, if any.
fn active_provider(config: &std::path::Path) -> Result<Option<String>> {
    let Some(text) = crate::fsutil::read_optional(config)? else {
        return Ok(None);
    };
    let table: toml::Table = anyhow::Context::with_context(toml::from_str(&text), || {
        format!("parse {}", config.display())
    })?;
    Ok(table
        .get("model_provider")
        .and_then(toml::Value::as_str)
        .map(str::to_owned))
}

#[cfg(test)]
mod tests;
