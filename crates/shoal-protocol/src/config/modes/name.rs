//! A mode's name: one of Shoal's built-in modes or one config defines. The
//! set is open, so any name loads, and resolving it against config decides
//! whether a mode of that name exists.
use std::borrow::Cow;

/// The name of a mode, as config, records and the protocol spell it.
#[derive(
    Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, serde::Serialize, serde::Deserialize,
)]
#[serde(transparent)]
pub struct ModeName(Cow<'static, str>);

impl ModeName {
    /// The mode `shoal start` uses without `--mode`.
    pub const DEFAULT: Self = Self::built_in("worker");
    /// The mode a chat starts in without `--mode`.
    pub const CHAT: Self = Self::built_in("chat");
    /// The mode whose `--issue` without a value offers the issues that need
    /// a plan.
    pub const PLAN: Self = Self::built_in("plan");
    /// The mode of a chat without a repository that manages every space.
    pub const ORCHESTRATE: Self = Self::built_in("orchestrate");
    /// The mode in which an agent writes Shoal's config, which `shoal repo
    /// setup` and `shoal config setup` start. It takes the rules of its
    /// [`super::setup_base`] with instructions of its own.
    pub const SETUP: Self = Self::built_in("setup");

    const fn built_in(name: &'static str) -> Self {
        Self(Cow::Borrowed(name))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl AsRef<str> for ModeName {
    fn as_ref(&self) -> &str {
        &self.0
    }
}

impl std::borrow::Borrow<str> for ModeName {
    fn borrow(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for ModeName {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.0)
    }
}

impl From<String> for ModeName {
    fn from(name: String) -> Self {
        Self(Cow::Owned(name))
    }
}

impl From<&str> for ModeName {
    fn from(name: &str) -> Self {
        Self(Cow::Owned(name.to_owned()))
    }
}

impl std::str::FromStr for ModeName {
    type Err = std::convert::Infallible;

    fn from_str(name: &str) -> Result<Self, Self::Err> {
        Ok(name.into())
    }
}

impl From<ModeName> for String {
    fn from(name: ModeName) -> Self {
        name.0.into_owned()
    }
}

impl From<ModeName> for std::ffi::OsString {
    fn from(name: ModeName) -> Self {
        name.0.into_owned().into()
    }
}

impl PartialEq<str> for ModeName {
    fn eq(&self, other: &str) -> bool {
        self.0 == other
    }
}

impl PartialEq<&str> for ModeName {
    fn eq(&self, other: &&str) -> bool {
        self.0 == *other
    }
}

#[cfg(feature = "rusqlite")]
impl rusqlite::types::ToSql for ModeName {
    fn to_sql(&self) -> rusqlite::Result<rusqlite::types::ToSqlOutput<'_>> {
        self.as_str().to_sql()
    }
}

#[cfg(feature = "rusqlite")]
impl rusqlite::types::FromSql for ModeName {
    fn column_result(value: rusqlite::types::ValueRef<'_>) -> rusqlite::types::FromSqlResult<Self> {
        String::column_result(value).map(Self::from)
    }
}
