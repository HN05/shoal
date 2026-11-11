//! How spaces are named and selected: a space's name, which is never all
//! digits, since a number alone selects a space by its number, and the
//! selector a command names a space with.
use crate::space_ids::{SpaceId, SpaceNumber};
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
}

/// The name of a directory or terminal session made from `text`, such as a
/// branch: ASCII letters, digits, `-` and `_`, with every other character
/// `-`, never starting with `-` or `_`, cut to the longest name allowed;
/// `workspace` when nothing is left.
pub fn directory_name(text: &str) -> String {
    let mapped: String = text
        .chars()
        .map(
            |c| match c.is_ascii_alphanumeric() || matches!(c, '-' | '_') {
                true => c,
                false => '-',
            },
        )
        .collect();
    match mapped.trim_start_matches(['-', '_']) {
        "" => "workspace".to_owned(),
        name => name.chars().take(MAX_NAME_LEN).collect(),
    }
}

/// `name`, or with `-<suffix>` from 2 on, cut to fit the longest name
/// allowed.
pub fn suffixed_directory(name: &str, suffix: u64) -> String {
    let suffix = match suffix {
        1 => String::new(),
        n => format!("-{n}"),
    };
    let kept = MAX_NAME_LEN.saturating_sub(suffix.len());
    let mut name: String = name.chars().take(kept).collect();
    name.push_str(&suffix);
    name
}

/// A space's number with its name, such as `3 Fix login`, as text that
/// names a space gives them, since names can repeat.
pub fn label(number: SpaceNumber, name: &SpaceName) -> String {
    format!("{number} {name}")
}

/// `names` joined by `separator`, as a message lists them.
pub fn join(names: &[SpaceName], separator: &str) -> String {
    let names: Vec<&str> = names.iter().map(SpaceName::as_str).collect();
    names.join(separator)
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
#[cfg(any(test, feature = "test-support"))]
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

/// What selects a space where a command takes one: its number, when all
/// digits, else its ID, or `-` for the previous space a command acted on.
/// A name never selects a space, since names can repeat.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub enum SpaceSelector {
    Number(SpaceNumber),
    Id(SpaceId),
    /// `-`, which the CLI resolves to the space it acted on before.
    Previous,
}

impl SpaceSelector {
    /// The text that stands for the previous space.
    pub const PREVIOUS: &str = "-";

    /// What `text` someone typed selects: its number when all digits,
    /// else its ID, refusing text that is neither.
    pub fn parse(text: &str) -> Result<Self> {
        let selector = Self::read(text);
        if let Self::Id(id) = &selector {
            anyhow::ensure!(
                uuid::Uuid::try_parse(id.as_str()).is_ok(),
                "a space is selected by its number or ID, not `{text}`"
            );
        }
        Ok(selector)
    }

    /// What recorded `text`, such as a request's, selects: its number when
    /// all digits, else the ID it gives, which selects nothing when no
    /// space has it.
    fn read(text: &str) -> Self {
        if text == Self::PREVIOUS {
            return Self::Previous;
        }
        match text.parse() {
            Ok(number) if all_digits(text) => Self::Number(number),
            _ => Self::Id(SpaceId::from(text.to_owned())),
        }
    }

    /// The space with the ID `id`.
    pub fn id(id: &SpaceId) -> Self {
        Self::Id(id.clone())
    }

    /// The ID it gives; none for a number, which selects only a space that
    /// exists, never a removed one.
    pub fn given_id(&self) -> Option<&SpaceId> {
        match self {
            Self::Id(id) => Some(id),
            Self::Number(_) | Self::Previous => None,
        }
    }

    /// Whether it selects the space with this ID and number.
    pub fn selects(&self, id: &SpaceId, number: SpaceNumber) -> bool {
        match self {
            Self::Number(selected) => *selected == number,
            Self::Id(selected) => selected == id,
            Self::Previous => false,
        }
    }
}

impl std::fmt::Display for SpaceSelector {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Number(number) => write!(f, "{number}"),
            Self::Id(id) => f.write_str(id.as_str()),
            Self::Previous => f.write_str(Self::PREVIOUS),
        }
    }
}

impl std::str::FromStr for SpaceSelector {
    type Err = anyhow::Error;

    fn from_str(value: &str) -> Result<Self> {
        Self::parse(value)
    }
}

/// Arguments parse as text someone typed, whatever other conversions a
/// build offers.
#[cfg(feature = "clap")]
impl clap::builder::ValueParserFactory for SpaceSelector {
    type Parser = fn(&str) -> Result<Self>;

    fn value_parser() -> Self::Parser {
        Self::parse
    }
}

/// A request's selector, which the client that sent it already checked.
impl TryFrom<String> for SpaceSelector {
    type Error = anyhow::Error;

    fn try_from(value: String) -> Result<Self> {
        anyhow::ensure!(!value.is_empty(), "a space is selected by its number or ID");
        Ok(Self::read(&value))
    }
}

impl From<SpaceSelector> for String {
    fn from(selector: SpaceSelector) -> Self {
        selector.to_string()
    }
}

impl From<&SpaceId> for SpaceSelector {
    fn from(id: &SpaceId) -> Self {
        Self::id(id)
    }
}

impl From<SpaceId> for SpaceSelector {
    fn from(id: SpaceId) -> Self {
        Self::Id(id)
    }
}

/// Selectors tests compare with the text that names them.
#[cfg(any(test, feature = "test-support"))]
impl PartialEq<str> for SpaceSelector {
    fn eq(&self, other: &str) -> bool {
        match self {
            Self::Number(number) => other.parse() == Ok(*number),
            Self::Id(id) => id == other,
            Self::Previous => other == Self::PREVIOUS,
        }
    }
}

#[cfg(any(test, feature = "test-support"))]
impl PartialEq<&str> for SpaceSelector {
    fn eq(&self, other: &&str) -> bool {
        <Self as PartialEq<str>>::eq(self, other)
    }
}

#[cfg(any(test, feature = "test-support"))]
impl PartialEq<String> for SpaceSelector {
    fn eq(&self, other: &String) -> bool {
        <Self as PartialEq<str>>::eq(self, other)
    }
}

/// Selectors tests write as literals: a number, or else an ID, which tests
/// need not write as one Shoal generates.
#[cfg(any(test, feature = "test-support"))]
impl From<&str> for SpaceSelector {
    fn from(value: &str) -> Self {
        Self::read(value)
    }
}

#[cfg(feature = "rusqlite")]
impl rusqlite::types::ToSql for SpaceName {
    fn to_sql(&self) -> rusqlite::Result<rusqlite::types::ToSqlOutput<'_>> {
        self.0.to_sql()
    }
}

#[cfg(feature = "rusqlite")]
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
