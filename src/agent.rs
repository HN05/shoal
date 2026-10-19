//! Shared agent identities and launch modes, independent of launch adapters.
pub mod state_hooks;

use crate::state::states;

states!(BuiltinAgent: Variants {
    Claude => "claude",
    Codex => "codex",
});

impl std::str::FromStr for BuiltinAgent {
    type Err = ();

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        Self::ALL
            .iter()
            .copied()
            .find(|agent| agent.as_str() == value)
            .ok_or(())
    }
}

states!(
    #[derive(Default)]
    CodexMode {
        #[default]
        Cli => "cli",
        App => "app",
    }
);

/// What `add --agent` starts: a terminal agent, or a detached Happy session
/// running one of Happy's agents, or a user-configured command.
#[derive(Debug, Clone, PartialEq, Eq, serde::Deserialize, serde::Serialize)]
#[serde(try_from = "String", into = "String")]
pub enum Agent {
    Codex,
    Claude,
    Happy(BuiltinAgent),
    Custom(String),
}

impl Agent {
    /// Built-in agent spellings, in help and completion order.
    pub fn possible_values() -> Vec<String> {
        let mut values = vec![
            BuiltinAgent::Codex.as_str().to_owned(),
            BuiltinAgent::Claude.as_str().to_owned(),
        ];
        values.extend(
            BuiltinAgent::ALL
                .iter()
                .map(|agent| format!("{HAPPY_PREFIX}{}", agent.as_str())),
        );
        values
    }
}

const HAPPY_PREFIX: &str = "happy-";

impl std::str::FromStr for Agent {
    type Err = ();

    fn from_str(value: &str) -> Result<Self, ()> {
        match value {
            "codex" => Ok(Agent::Codex),
            "claude" => Ok(Agent::Claude),
            _ if value.starts_with(HAPPY_PREFIX) => value
                .strip_prefix(HAPPY_PREFIX)
                .and_then(|agent| agent.parse().ok())
                .map(Agent::Happy)
                .ok_or(()),
            _ if value != "happy" && crate::validate::lowercase_name("agent", value).is_ok() => {
                Ok(Agent::Custom(value.to_owned()))
            }
            _ => Err(()),
        }
    }
}

/// Config spelling: the same values `--agent` accepts.
impl TryFrom<String> for Agent {
    type Error = String;

    fn try_from(value: String) -> Result<Self, String> {
        value.parse().map_err(|()| {
            format!(
                "invalid agent {value:?}; use a configured command name or one of {}",
                Agent::possible_values().join(", ")
            )
        })
    }
}

impl From<Agent> for String {
    fn from(agent: Agent) -> Self {
        match agent {
            Agent::Codex => BuiltinAgent::Codex.as_str().into(),
            Agent::Claude => BuiltinAgent::Claude.as_str().into(),
            Agent::Happy(agent) => format!("{HAPPY_PREFIX}{}", agent.as_str()),
            Agent::Custom(name) => name,
        }
    }
}

/// Agent hook events that receive undelivered agent messages as context:
/// after each tool call, and when the user submits a prompt.
const MESSAGE_HOOK_EVENTS: [&str; 2] = ["PostToolUse", "UserPromptSubmit"];

/// How long the Claude Code wake hook may wait for an update: a week, since
/// an idle agent should hear about a review whenever it arrives.
const WAKE_TIMEOUT_SECS: u64 = 7 * 24 * 60 * 60;

/// The Claude Code `Stop` group that waits in the background for the next
/// update and wakes the idle agent with it. Claude Code runs the command in a
/// shell it starts, whose parent is the agent.
fn claude_wake_hook(shoal: &str) -> (&'static str, serde_json::Value) {
    (
        "Stop",
        serde_json::json!({"hooks": [{
            "type": "command",
            "command": format!("{shoal} internal wake --agent \"$PPID\""),
            "asyncRewake": true,
            "timeout": WAKE_TIMEOUT_SECS,
        }]}),
    )
}

impl BuiltinAgent {
    /// Arguments that register Shoal's hooks for this agent's session:
    /// `shoal messages --hook`, its turn state, and for Claude Code the wake
    /// hook. The hooks inherit the execution's scope and state directory.
    pub fn hook_args(self) -> std::io::Result<Vec<std::ffi::OsString>> {
        let shoal = crate::fsutil::invoked_executable()?;
        let shoal = crate::shell::quote(&[&shoal.to_string_lossy()]);
        let command = format!("{shoal} messages --hook");
        let handler = serde_json::json!({"type": "command", "command": command, "timeout": 10});
        let messages = MESSAGE_HOOK_EVENTS
            .iter()
            .map(|event| (*event, serde_json::json!({"hooks": [handler]})));
        Ok(match self {
            Self::Claude => {
                // One `--settings` carries every hook; Claude Code merges it
                // with the user's own settings.
                let groups = messages
                    .chain(state_hooks::claude(&shoal))
                    .chain([claude_wake_hook(&shoal)]);
                vec![
                    "--settings".into(),
                    serde_json::json!({"hooks": by_event(groups)})
                        .to_string()
                        .into(),
                ]
            }
            Self::Codex => by_event(messages.chain(state_hooks::codex(&shoal)))
                .into_iter()
                .flat_map(|(event, groups)| {
                    let groups: toml::Value =
                        serde_json::from_value(groups).expect("hook groups are valid TOML");
                    ["-c".into(), format!("hooks.{event}={groups}").into()]
                })
                .collect(),
        })
    }
}

/// Hook groups collected under their events, in order.
fn by_event(
    groups: impl IntoIterator<Item = (&'static str, serde_json::Value)>,
) -> serde_json::Map<String, serde_json::Value> {
    let mut hooks = serde_json::Map::new();
    for (event, group) in groups {
        if let serde_json::Value::Array(groups) =
            hooks.entry(event).or_insert_with(|| serde_json::json!([]))
        {
            groups.push(group);
        }
    }
    hooks
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn message_hooks_run_shoal_after_tool_calls_and_prompts() {
        let claude = BuiltinAgent::Claude.hook_args().unwrap();
        assert_eq!(claude[0], "--settings");
        let settings: serde_json::Value =
            serde_json::from_str(claude[1].to_str().unwrap()).unwrap();
        for event in MESSAGE_HOOK_EVENTS {
            let command = settings["hooks"][event][0]["hooks"][0]["command"]
                .as_str()
                .unwrap();
            assert!(command.ends_with(" messages --hook"), "{command}");
        }
        // The same settings report Claude Code's turn state.
        let stop = settings["hooks"]["Stop"][0]["hooks"][0]["command"]
            .as_str()
            .unwrap();
        assert!(stop.contains(" agent-state idle "), "{stop}");
        // A second Stop group waits in the background to wake the idle agent.
        let wake = &settings["hooks"]["Stop"][1]["hooks"][0];
        assert!(
            wake["command"]
                .as_str()
                .unwrap()
                .ends_with(" internal wake --agent \"$PPID\""),
            "{wake}"
        );
        assert_eq!(wake["asyncRewake"], true);
        assert_eq!(wake["timeout"], WAKE_TIMEOUT_SECS);
        let codex = BuiltinAgent::Codex.hook_args().unwrap();
        let hooks: std::collections::BTreeMap<_, _> = codex
            .chunks(2)
            .map(|flag| {
                assert_eq!(flag[0], "-c");
                let (key, value) = flag[1].to_str().unwrap().split_once('=').unwrap();
                let parsed: toml::Table = toml::from_str(&format!("value = {value}")).unwrap();
                (key.to_owned(), parsed["value"].clone())
            })
            .collect();
        assert_eq!(
            hooks.keys().collect::<Vec<_>>(),
            ["hooks.PostToolUse", "hooks.Stop", "hooks.UserPromptSubmit"]
        );
        for event in MESSAGE_HOOK_EVENTS {
            let groups = &hooks[&format!("hooks.{event}")];
            let handler = &groups[0]["hooks"][0];
            assert_eq!(handler["type"].as_str(), Some("command"));
            assert!(
                handler["command"]
                    .as_str()
                    .unwrap()
                    .ends_with(" messages --hook")
            );
            let state = groups[1]["hooks"][0]["command"].as_str().unwrap();
            assert!(state.contains(" agent-state working "), "{state}");
        }
        let stop = hooks["hooks.Stop"][0]["hooks"][0]["command"]
            .as_str()
            .unwrap();
        assert!(stop.contains(" agent-state idle "), "{stop}");
    }
}
