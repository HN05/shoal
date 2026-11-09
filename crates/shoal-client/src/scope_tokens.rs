//! A scope token: the secret the daemon issues a space's agent, commands
//! and workers, which grants that space's scope to whoever presents it. Its
//! `Debug` redacts it, so a log line or error context that prints a struct
//! holding one never shows it; sending or comparing its value reads it with
//! [`ScopeToken::expose`].
use serde::{Deserialize, Serialize};

/// A scope token, stored and serialized as its text.
#[derive(Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct ScopeToken(String);

impl ScopeToken {
    /// A new token, which no other caller holds.
    pub fn generate() -> Self {
        Self(uuid::Uuid::new_v4().to_string())
    }

    /// The token's text, for the places that send or compare it.
    pub fn expose(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Debug for ScopeToken {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("ScopeToken(<redacted>)")
    }
}

/// The token that recorded text, such as an environment variable or a file
/// a starter hands over, holds.
impl From<String> for ScopeToken {
    fn from(token: String) -> Self {
        Self(token)
    }
}

/// Tokens tests write as literals.
#[cfg(any(test, feature = "test-support"))]
impl From<&str> for ScopeToken {
    fn from(token: &str) -> Self {
        Self(token.to_owned())
    }
}

/// The token a command-line argument gives.
impl std::str::FromStr for ScopeToken {
    type Err = std::convert::Infallible;

    fn from_str(token: &str) -> Result<Self, Self::Err> {
        Ok(Self(token.to_owned()))
    }
}

#[cfg(feature = "rusqlite")]
impl rusqlite::types::ToSql for ScopeToken {
    fn to_sql(&self) -> rusqlite::Result<rusqlite::types::ToSqlOutput<'_>> {
        self.0.to_sql()
    }
}

#[cfg(feature = "rusqlite")]
impl rusqlite::types::FromSql for ScopeToken {
    fn column_result(value: rusqlite::types::ValueRef<'_>) -> rusqlite::types::FromSqlResult<Self> {
        String::column_result(value).map(Self)
    }
}
