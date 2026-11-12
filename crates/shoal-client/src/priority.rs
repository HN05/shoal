//! How much a space's work matters against other work: a number from 0 to
//! 100, higher first, which ranks the agent queue ahead of everything else.
use std::{fmt, str::FromStr};

use anyhow::{Context, Result, ensure};
use serde::{Deserialize, Serialize};

use crate::{
    connection::{Daemon, request},
    protocol::QueueMethod,
    space_names::{SpaceName, SpaceSelector},
};

/// A priority, from 0 to 100; [`Self::NORMAL`] unless set.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(try_from = "u8", into = "u8")]
pub struct Priority(u8);

impl Priority {
    pub const LOW: Self = Self(25);
    pub const NORMAL: Self = Self(50);
    pub const URGENT: Self = Self(75);
    const HIGHEST: u8 = 100;
    /// The names that stand for values.
    const NAMES: [(&'static str, Self); 3] = [
        ("low", Self::LOW),
        ("normal", Self::NORMAL),
        ("urgent", Self::URGENT),
    ];

    pub fn value(self) -> u8 {
        self.0
    }
}

impl Default for Priority {
    fn default() -> Self {
        Self::NORMAL
    }
}

impl TryFrom<u8> for Priority {
    type Error = anyhow::Error;

    fn try_from(value: u8) -> Result<Self> {
        ensure!(
            value <= Self::HIGHEST,
            "a priority is at most {}, not {value}",
            Self::HIGHEST
        );
        Ok(Self(value))
    }
}

impl From<Priority> for u8 {
    fn from(priority: Priority) -> Self {
        priority.0
    }
}

impl FromStr for Priority {
    type Err = anyhow::Error;

    /// A name, or a number from 0 to 100.
    fn from_str(text: &str) -> Result<Self> {
        if let Some((_, priority)) = Self::NAMES.iter().find(|(name, _)| *name == text) {
            return Ok(*priority);
        }
        let value: u8 = text.parse().with_context(|| {
            format!(
                "{text:?} is not a priority: give low, normal, urgent or a number from 0 to 100"
            )
        })?;
        value.try_into()
    }
}

impl fmt::Display for Priority {
    /// Its name, or its number when it has none.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match Self::NAMES.iter().find(|(_, priority)| priority == self) {
            Some((name, _)) => f.write_str(name),
            None => write!(f, "{}", self.0),
        }
    }
}

/// A space's priority after `shoal priority`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Prioritized {
    pub space: SpaceName,
    pub priority: Priority,
}

/// Set `priority` on the space `space` names, which ranks its queued work.
pub async fn set(
    daemon: &impl Daemon,
    space: SpaceSelector,
    priority: Priority,
) -> Result<Prioritized> {
    request(daemon, QueueMethod::SetPriority { space, priority }).await
}

#[cfg(feature = "rusqlite")]
impl rusqlite::types::ToSql for Priority {
    fn to_sql(&self) -> rusqlite::Result<rusqlite::types::ToSqlOutput<'_>> {
        Ok(rusqlite::types::ToSqlOutput::from(self.0))
    }
}

#[cfg(feature = "rusqlite")]
impl rusqlite::types::FromSql for Priority {
    fn column_result(value: rusqlite::types::ValueRef<'_>) -> rusqlite::types::FromSqlResult<Self> {
        let value = u8::column_result(value)?;
        Self::try_from(value).map_err(|error| rusqlite::types::FromSqlError::Other(error.into()))
    }
}

#[cfg(test)]
mod tests;
