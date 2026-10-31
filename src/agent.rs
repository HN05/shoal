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

/// What `add --agent` and `agent.default` start: an AI tool, a configured
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

/// The model and reasoning effort an agent runs with; each one left out
/// keeps the tool's own. Values pass to the tool unchanged, which checks
/// them.
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Deserialize, serde::Serialize)]
pub struct ModelChoice {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub effort: Option<String>,
}

impl ModelChoice {
    /// Whether it leaves both to the tool.
    pub fn is_empty(&self) -> bool {
        self.model.is_none() && self.effort.is_none()
    }

    /// Refuse an empty value or one with NUL, which no argument can carry;
    /// `section` is where they are set.
    pub fn validate(&self, section: &str) -> anyhow::Result<()> {
        for (key, value) in [("model", &self.model), ("effort", &self.effort)] {
            anyhow::ensure!(
                value
                    .as_deref()
                    .is_none_or(|value| !value.is_empty() && !value.contains('\0')),
                "{section}.{key} must be nonempty and must not contain NUL"
            );
        }
        Ok(())
    }

    /// Each value it sets, else `fallback`'s.
    pub fn or(self, fallback: Self) -> Self {
        Self {
            model: self.model.or(fallback.model),
            effort: self.effort.or(fallback.effort),
        }
    }
}
