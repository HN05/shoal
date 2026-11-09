//! Agent names and the models they run with, independent of the integrations
//! that launch them.

/// What `add --agent` and `agent.default` start: an AI tool, a configured
/// command, or a name an integration serves. Parsing validates the name;
/// a record loads the name it holds unchanged, so one saved before names
/// were validated still loads.
#[derive(
    Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, serde::Deserialize, serde::Serialize,
)]
#[serde(transparent)]
pub struct Agent(String);

impl Agent {
    /// The agent `name` names without validating it: a name a record holds,
    /// which may predate validation, or one Shoal gives itself.
    pub fn unchecked(name: String) -> Self {
        Self(name)
    }

    pub fn name(&self) -> &str {
        &self.0
    }

    /// The agent as `--agent` and recovery records name it: a name an app's
    /// execution records with a space, such as `happy claude`, takes `-`
    /// instead.
    pub fn normalized(&self) -> Self {
        Self(self.0.replace(' ', "-"))
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
pub fn configured<'de, D: serde::Deserializer<'de>>(
    deserializer: D,
) -> Result<Option<Agent>, D::Error> {
    <Option<String> as serde::Deserialize>::deserialize(deserializer)?
        .map(|name| {
            name.parse().map_err(|error| {
                serde::de::Error::custom(format!("invalid agent {name:?}: {error}"))
            })
        })
        .transpose()
}

impl From<Agent> for String {
    fn from(agent: Agent) -> Self {
        agent.0
    }
}

impl From<Agent> for std::ffi::OsString {
    fn from(agent: Agent) -> Self {
        agent.0.into()
    }
}

impl AsRef<str> for Agent {
    fn as_ref(&self) -> &str {
        &self.0
    }
}

impl AsRef<std::ffi::OsStr> for Agent {
    fn as_ref(&self) -> &std::ffi::OsStr {
        self.0.as_ref()
    }
}

impl PartialEq<str> for Agent {
    fn eq(&self, other: &str) -> bool {
        self.0 == other
    }
}

impl PartialEq<&str> for Agent {
    fn eq(&self, other: &&str) -> bool {
        self.0 == *other
    }
}

/// Agents tests write as literals.
#[cfg(any(test, feature = "test-support"))]
impl From<&str> for Agent {
    fn from(name: &str) -> Self {
        Self(name.to_owned())
    }
}

#[cfg(feature = "rusqlite")]
impl rusqlite::types::ToSql for Agent {
    fn to_sql(&self) -> rusqlite::Result<rusqlite::types::ToSqlOutput<'_>> {
        self.0.to_sql()
    }
}

#[cfg(feature = "rusqlite")]
impl rusqlite::types::FromSql for Agent {
    fn column_result(value: rusqlite::types::ValueRef<'_>) -> rusqlite::types::FromSqlResult<Self> {
        String::column_result(value).map(Self)
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
