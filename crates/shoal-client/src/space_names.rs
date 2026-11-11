//! How spaces are named and selected: a space's name, the free-text label
//! people read, and the selector a command takes a space by, its number or
//! its ID, since names can repeat.
use crate::space_ids::{SpaceId, SpaceNumber};
use anyhow::{Result, ensure};
use serde::{Deserialize, Serialize};

use crate::validate::MAX_NAME_LEN;

/// The most characters a space's name holds, so it fits a tab whatever its
/// script.
pub const MAX_SPACE_NAME_LEN: usize = 40;

/// A space's name: the one label people read, a single line of 1–40
/// characters without control characters or bidirectional overrides, with
/// no leading or trailing whitespace and single spaces inside. It never
/// selects a space and nothing is built from it, so names may repeat. A
/// name a record holds reads as it was stored, whatever rule it predates.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(from = "String", into = "String")]
pub struct SpaceName(String);

impl SpaceName {
    /// The name someone sets, with its whitespace trimmed and collapsed,
    /// refused when it is empty, longer than the limit, or holds a control
    /// character or a bidirectional override.
    pub fn parse(value: &str) -> Result<Self> {
        ensure!(
            !value.chars().any(forbidden),
            "a space's name is one line without control characters or bidirectional overrides"
        );
        let name = collapsed(value);
        ensure!(!name.is_empty(), "a space's name cannot be empty");
        ensure!(
            name.chars().count() <= MAX_SPACE_NAME_LEN,
            "a space's name is at most {MAX_SPACE_NAME_LEN} characters"
        );
        Ok(Self(name))
    }

    /// The name Shoal gives a space from `text`, such as a branch or an
    /// issue's title: what [`Self::parse`] refuses becomes a space, and a
    /// name over the limit is cut to end with `…`; `fallback` when nothing
    /// is left.
    pub fn derive(text: &str, fallback: &str) -> Self {
        let cleaned: String = (text.chars())
            .map(|c| if forbidden(c) { ' ' } else { c })
            .collect();
        let name = match collapsed(&cleaned) {
            name if name.is_empty() => fallback.to_owned(),
            name => name,
        };
        Self(shortened(name))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// Whether `c` cannot be part of a name: a control character, which would
/// corrupt the terminals that show it, or a bidirectional override, which
/// would make it read as other text.
fn forbidden(c: char) -> bool {
    c.is_control() || matches!(c, '\u{202A}'..='\u{202E}' | '\u{2066}'..='\u{2069}')
}

/// `text` without leading or trailing whitespace and with each run inside it
/// one space.
fn collapsed(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// `name`, or cut to the limit ending with `…` when it is longer.
fn shortened(name: String) -> String {
    if name.chars().count() <= MAX_SPACE_NAME_LEN {
        return name;
    }
    let kept: String = name.chars().take(MAX_SPACE_NAME_LEN - 1).collect();
    format!("{}…", kept.trim_end())
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

/// Arguments parse as a name someone sets, whatever other conversions a
/// build offers.
#[cfg(feature = "clap")]
impl clap::builder::ValueParserFactory for SpaceName {
    type Parser = fn(&str) -> Result<Self>;

    fn value_parser() -> Self::Parser {
        Self::parse
    }
}

/// A name a record holds, as it was stored.
impl From<String> for SpaceName {
    fn from(name: String) -> Self {
        Self(name)
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
        Ok(Self(value.as_str()?.to_owned()))
    }
}

#[cfg(test)]
mod tests;
