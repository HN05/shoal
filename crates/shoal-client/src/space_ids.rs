//! A space's ID: the key the daemon gives a workspace or chat when it
//! creates it, which records, the protocol and the store refer to it by. It
//! is never a name or a selector's text, which name a space only until the
//! daemon resolves them.
use serde::{Deserialize, Serialize};

/// The ID of a workspace or chat, stored and serialized as its text.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(transparent)]
pub struct SpaceId(String);

impl SpaceId {
    /// A new space's ID.
    pub fn generate() -> Self {
        Self(uuid::Uuid::new_v4().to_string())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::ops::Deref for SpaceId {
    type Target = str;

    fn deref(&self) -> &str {
        &self.0
    }
}

impl AsRef<str> for SpaceId {
    fn as_ref(&self) -> &str {
        &self.0
    }
}

impl AsRef<std::ffi::OsStr> for SpaceId {
    fn as_ref(&self) -> &std::ffi::OsStr {
        self.0.as_ref()
    }
}

impl AsRef<std::path::Path> for SpaceId {
    fn as_ref(&self) -> &std::path::Path {
        self.0.as_ref()
    }
}

impl std::borrow::Borrow<str> for SpaceId {
    fn borrow(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for SpaceId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

/// The ID that recorded text, such as an environment variable or an
/// argument Shoal passes itself, holds.
impl From<String> for SpaceId {
    fn from(id: String) -> Self {
        Self(id)
    }
}

impl std::str::FromStr for SpaceId {
    type Err = std::convert::Infallible;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        Ok(Self(value.to_owned()))
    }
}

/// IDs tests write as literals.
#[cfg(any(test, feature = "test-support"))]
impl From<&str> for SpaceId {
    fn from(value: &str) -> Self {
        Self(value.to_owned())
    }
}

impl From<SpaceId> for String {
    fn from(id: SpaceId) -> Self {
        id.0
    }
}

impl From<SpaceId> for std::ffi::OsString {
    fn from(id: SpaceId) -> Self {
        id.0.into()
    }
}

impl PartialEq<str> for SpaceId {
    fn eq(&self, other: &str) -> bool {
        self.0 == other
    }
}

impl PartialEq<&str> for SpaceId {
    fn eq(&self, other: &&str) -> bool {
        self.0 == *other
    }
}

impl PartialEq<String> for SpaceId {
    fn eq(&self, other: &String) -> bool {
        &self.0 == other
    }
}

impl PartialEq<SpaceId> for str {
    fn eq(&self, other: &SpaceId) -> bool {
        self == other.0
    }
}

impl PartialEq<SpaceId> for &str {
    fn eq(&self, other: &SpaceId) -> bool {
        *self == other.0
    }
}

impl PartialEq<SpaceId> for String {
    fn eq(&self, other: &SpaceId) -> bool {
        *self == other.0
    }
}

#[cfg(feature = "rusqlite")]
impl rusqlite::types::ToSql for SpaceId {
    fn to_sql(&self) -> rusqlite::Result<rusqlite::types::ToSqlOutput<'_>> {
        self.0.to_sql()
    }
}

#[cfg(feature = "rusqlite")]
impl rusqlite::types::FromSql for SpaceId {
    fn column_result(value: rusqlite::types::ValueRef<'_>) -> rusqlite::types::FromSqlResult<Self> {
        String::column_result(value).map(Self)
    }
}
