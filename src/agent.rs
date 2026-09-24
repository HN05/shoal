//! Shared agent identities and launch modes, independent of launch adapters.
use crate::state::states;

states!(BuiltinAgent {
    Claude => "claude",
    Codex => "codex",
});

impl BuiltinAgent {
    pub const ALL: &[Self] = &[Self::Claude, Self::Codex];
}

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
