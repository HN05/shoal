//! Shared agent identities and launch modes, independent of launch adapters.
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

impl BuiltinAgent {
    /// Arguments that register `shoal messages --hook` for this agent's
    /// session. The hook inherits the execution's scope and state directory.
    pub fn message_hook_args(self) -> std::io::Result<Vec<std::ffi::OsString>> {
        let shoal = crate::fsutil::invoked_executable()?;
        let command = format!(
            "{} messages --hook",
            crate::shell::quote(&[&shoal.to_string_lossy()])
        );
        let handler = serde_json::json!({"type": "command", "command": command, "timeout": 10});
        Ok(match self {
            Self::Claude => {
                let hooks: serde_json::Map<_, _> = MESSAGE_HOOK_EVENTS
                    .iter()
                    .map(|event| ((*event).into(), serde_json::json!([{"hooks": [handler]}])))
                    .collect();
                vec![
                    "--settings".into(),
                    serde_json::json!({"hooks": hooks}).to_string().into(),
                ]
            }
            Self::Codex => {
                let groups: toml::Value =
                    serde_json::from_value(serde_json::json!([{"hooks": [handler]}]))
                        .expect("hook groups are valid TOML");
                MESSAGE_HOOK_EVENTS
                    .iter()
                    .flat_map(|event| ["-c".into(), format!("hooks.{event}={groups}").into()])
                    .collect()
            }
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn message_hooks_run_shoal_after_tool_calls_and_prompts() {
        let claude = BuiltinAgent::Claude.message_hook_args().unwrap();
        assert_eq!(claude[0], "--settings");
        let settings: serde_json::Value =
            serde_json::from_str(claude[1].to_str().unwrap()).unwrap();
        for event in MESSAGE_HOOK_EVENTS {
            let command = settings["hooks"][event][0]["hooks"][0]["command"]
                .as_str()
                .unwrap();
            assert!(command.ends_with(" messages --hook"), "{command}");
        }
        let codex = BuiltinAgent::Codex.message_hook_args().unwrap();
        assert_eq!(codex.len(), 4);
        for (flag, event) in codex.chunks(2).zip(MESSAGE_HOOK_EVENTS) {
            assert_eq!(flag[0], "-c");
            let (key, value) = flag[1].to_str().unwrap().split_once('=').unwrap();
            assert_eq!(key, format!("hooks.{event}"));
            let parsed: toml::Table = toml::from_str(&format!("value = {value}")).unwrap();
            let handler = &parsed["value"][0]["hooks"][0];
            assert_eq!(handler["type"].as_str(), Some("command"));
            assert!(
                handler["command"]
                    .as_str()
                    .unwrap()
                    .ends_with(" messages --hook")
            );
        }
    }
}
