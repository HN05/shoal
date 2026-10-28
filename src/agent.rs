//! Agent names and the hooks agents run Shoal through, independent of the
//! integrations that launch them.
pub mod hooks;

use crate::state::states;

states!(
    #[derive(Default)]
    AgentMode {
        #[default]
        Cli => "cli",
        App => "app",
    }
);

/// What `add --agent` and `default_agent` start: an AI tool, a configured
/// command, or a name an integration serves.
#[derive(Debug, Clone, PartialEq, Eq, Hash, serde::Deserialize, serde::Serialize)]
#[serde(try_from = "String", into = "String")]
pub struct Agent(String);

impl Agent {
    pub fn name(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for Agent {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.0)
    }
}

impl std::str::FromStr for Agent {
    type Err = anyhow::Error;

    fn from_str(value: &str) -> anyhow::Result<Self> {
        crate::validate::lowercase_name("agent", value)?;
        Ok(Self(value.to_owned()))
    }
}

/// Config spelling: the same values `--agent` accepts.
impl TryFrom<String> for Agent {
    type Error = String;

    fn try_from(value: String) -> Result<Self, String> {
        value
            .parse()
            .map_err(|error| format!("invalid agent {value:?}: {error}"))
    }
}

impl From<Agent> for String {
    fn from(agent: Agent) -> Self {
        agent.0
    }
}
