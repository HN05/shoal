//! How spaces are named: a space's name, which is never all digits, since a
//! number alone selects a space by its number.
use anyhow::{Result, bail};
use serde::{Deserialize, Serialize};

use crate::{model::SpaceKind, validate::MAX_NAME_LEN};

/// A space's name: 1–64 ASCII letters, digits, hyphens or underscores,
/// starting with a letter or digit, and not all digits.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct SpaceName(String);

impl SpaceName {
    /// The name someone gives, refused unless it is a valid space name.
    pub fn parse(value: &str) -> Result<Self> {
        crate::validate::name("space", value)?;
        if all_digits(value) {
            bail!("space names cannot be all digits; `{value}` selects space number {value}");
        }
        Ok(Self(value.to_owned()))
    }

    /// The name a space of `kind` derives from `text`, such as a branch:
    /// every other character becomes `-`, and a name that would be all digits
    /// or empty takes the kind as its prefix or name.
    pub fn derive(kind: SpaceKind, text: &str) -> Self {
        let mapped: String = text
            .chars()
            .map(
                |c| match c.is_ascii_alphanumeric() || matches!(c, '-' | '_') {
                    true => c,
                    false => '-',
                },
            )
            .collect();
        let mapped = mapped.trim_start_matches(['-', '_']);
        let name = match mapped {
            "" => kind.as_str().to_owned(),
            digits if all_digits(digits) => format!("{}-{digits}", kind.as_str()),
            name => name.to_owned(),
        };
        Self(name.chars().take(MAX_NAME_LEN).collect())
    }

    /// This name, or with `-<suffix>` from 2 on, shortened to fit the name
    /// limit; the suffix keeps it from being all digits.
    pub fn suffixed(&self, suffix: u64) -> Self {
        let suffix = match suffix {
            1 => String::new(),
            n => format!("-{n}"),
        };
        let kept = MAX_NAME_LEN.saturating_sub(suffix.len());
        let mut name: String = self.0.chars().take(kept).collect();
        name.push_str(&suffix);
        Self(name)
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// The name of the seed build's identity, `.seed`, which no space can
    /// hold, so what `[env]` derives from it never clashes with a space's.
    #[cfg(feature = "cli")]
    pub(crate) fn seed() -> Self {
        Self(".seed".to_owned())
    }
}

fn all_digits(value: &str) -> bool {
    !value.is_empty() && value.bytes().all(|c| c.is_ascii_digit())
}

impl std::ops::Deref for SpaceName {
    type Target = str;

    fn deref(&self) -> &str {
        &self.0
    }
}

impl AsRef<str> for SpaceName {
    fn as_ref(&self) -> &str {
        &self.0
    }
}

impl AsRef<std::ffi::OsStr> for SpaceName {
    fn as_ref(&self) -> &std::ffi::OsStr {
        self.0.as_ref()
    }
}

impl AsRef<std::path::Path> for SpaceName {
    fn as_ref(&self) -> &std::path::Path {
        self.0.as_ref()
    }
}

impl From<SpaceName> for std::ffi::OsString {
    fn from(name: SpaceName) -> Self {
        name.0.into()
    }
}

impl std::fmt::Display for SpaceName {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::str::FromStr for SpaceName {
    type Err = anyhow::Error;

    fn from_str(value: &str) -> Result<Self> {
        Self::parse(value)
    }
}

impl TryFrom<String> for SpaceName {
    type Error = anyhow::Error;

    fn try_from(value: String) -> Result<Self> {
        Self::parse(&value)
    }
}

/// Names tests write as literals, which must be valid.
#[cfg(test)]
impl From<&str> for SpaceName {
    fn from(value: &str) -> Self {
        Self::parse(value).expect("a valid space name")
    }
}

impl From<SpaceName> for String {
    fn from(name: SpaceName) -> Self {
        name.0
    }
}

impl PartialEq<str> for SpaceName {
    fn eq(&self, other: &str) -> bool {
        self.0 == other
    }
}

impl PartialEq<&str> for SpaceName {
    fn eq(&self, other: &&str) -> bool {
        self.0 == *other
    }
}

impl PartialEq<String> for SpaceName {
    fn eq(&self, other: &String) -> bool {
        &self.0 == other
    }
}

impl PartialEq<SpaceName> for str {
    fn eq(&self, other: &SpaceName) -> bool {
        self == other.0
    }
}

impl PartialEq<SpaceName> for &str {
    fn eq(&self, other: &SpaceName) -> bool {
        *self == other.0
    }
}

impl PartialEq<SpaceName> for String {
    fn eq(&self, other: &SpaceName) -> bool {
        *self == other.0
    }
}

#[cfg(feature = "cli")]
impl rusqlite::types::ToSql for SpaceName {
    fn to_sql(&self) -> rusqlite::Result<rusqlite::types::ToSqlOutput<'_>> {
        self.0.to_sql()
    }
}

#[cfg(feature = "cli")]
impl rusqlite::types::FromSql for SpaceName {
    fn column_result(value: rusqlite::types::ValueRef<'_>) -> rusqlite::types::FromSqlResult<Self> {
        Self::parse(value.as_str()?).map_err(|error| {
            rusqlite::types::FromSqlError::Other(Box::new(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                format!("{error:#}"),
            )))
        })
    }
}

#[cfg(test)]
mod tests;
