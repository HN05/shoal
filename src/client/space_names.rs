//! How spaces are named and selected: a space's name, which is never all
//! digits, since a number alone selects a space by its number, and the
//! selector a command names a space with.
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

/// What names a space where a command takes one: its number, when all
/// digits, else its ID or name. Text reads as one of those two. Within one
/// process, a stored ID that code passes on selects by ID alone, so it never
/// reads as a name; sent to the daemon, it is text again.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub enum SpaceSelector {
    Number(u32),
    IdOrName(String),
    Id(String),
}

impl SpaceSelector {
    /// What `text` selects: its number when all digits, else its ID or
    /// name. Empty text selects nothing.
    pub fn parse(text: &str) -> Self {
        match text.parse() {
            Ok(number) if all_digits(text) => Self::Number(number),
            _ => Self::IdOrName(text.to_owned()),
        }
    }

    /// The space with the ID `id`.
    pub fn id(id: &str) -> Self {
        Self::Id(id.to_owned())
    }

    /// The ID or name it gives; none for a number, which selects only a
    /// space that exists, never a removed one.
    pub fn id_or_name(&self) -> Option<&str> {
        match self {
            Self::Number(_) => None,
            Self::IdOrName(text) | Self::Id(text) => Some(text),
        }
    }

    /// Whether it gives this ID or name, as it selects a removed space,
    /// which has no number.
    pub fn names(&self, id: &str, name: &str) -> bool {
        match self {
            Self::Number(_) => false,
            Self::IdOrName(text) => text == id || text == name,
            Self::Id(text) => text == id,
        }
    }

    /// Whether it selects the space with this ID, name and number.
    pub fn selects(&self, id: &str, name: &str, number: u32) -> bool {
        match self {
            Self::Number(selected) => *selected == number,
            _ => self.names(id, name),
        }
    }
}

impl std::fmt::Display for SpaceSelector {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Number(number) => write!(f, "{number}"),
            Self::IdOrName(text) | Self::Id(text) => f.write_str(text),
        }
    }
}

impl std::str::FromStr for SpaceSelector {
    type Err = anyhow::Error;

    fn from_str(value: &str) -> Result<Self> {
        anyhow::ensure!(
            !value.is_empty(),
            "a space is named by its number, name or ID"
        );
        Ok(Self::parse(value))
    }
}

impl TryFrom<String> for SpaceSelector {
    type Error = anyhow::Error;

    fn try_from(value: String) -> Result<Self> {
        value.parse()
    }
}

impl From<SpaceSelector> for String {
    fn from(selector: SpaceSelector) -> Self {
        selector.to_string()
    }
}

impl From<&SpaceName> for SpaceSelector {
    fn from(name: &SpaceName) -> Self {
        Self::IdOrName(name.0.clone())
    }
}

impl From<SpaceName> for SpaceSelector {
    fn from(name: SpaceName) -> Self {
        Self::IdOrName(name.0)
    }
}

/// Selectors tests compare with the text that names them.
#[cfg(test)]
impl PartialEq<str> for SpaceSelector {
    fn eq(&self, other: &str) -> bool {
        match self {
            Self::Number(number) => other.parse() == Ok(*number),
            Self::IdOrName(text) | Self::Id(text) => text == other,
        }
    }
}

#[cfg(test)]
impl PartialEq<&str> for SpaceSelector {
    fn eq(&self, other: &&str) -> bool {
        <Self as PartialEq<str>>::eq(self, other)
    }
}

#[cfg(test)]
impl PartialEq<String> for SpaceSelector {
    fn eq(&self, other: &String) -> bool {
        <Self as PartialEq<str>>::eq(self, other)
    }
}

/// Selectors tests write as literals.
#[cfg(test)]
impl From<&str> for SpaceSelector {
    fn from(value: &str) -> Self {
        Self::parse(value)
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
