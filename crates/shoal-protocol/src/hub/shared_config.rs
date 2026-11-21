//! The shared config's files as joined machines share them through the hub:
//! `shared.toml` and the mode and rule files beside it, each by its name.
//! A machine sends the hub its edits; the hub keeps the last edit of each
//! file that reached it, under a revision that orders its copies, and sends
//! it to every machine.
use std::{fmt, str::FromStr};

use anyhow::{Result, bail};
use serde::{Deserialize, Serialize};

/// A shared file, as its name below the shared config's directory reads:
/// `shared.toml`, `modes/<name>.md` or `rules/<name>.md`.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub enum FileName {
    /// `shared.toml`.
    Config,
    /// The instructions of the mode it names.
    Mode(String),
    /// The text of the rule it names.
    Rule(String),
}

impl FileName {
    const CONFIG: &str = "shared.toml";
}

impl FromStr for FileName {
    type Err = anyhow::Error;

    fn from_str(text: &str) -> Result<Self> {
        if text == Self::CONFIG {
            return Ok(Self::Config);
        }
        let named = |prefix: &str, kind: &str| -> Result<Option<String>> {
            let Some(name) = (text.strip_prefix(prefix)).and_then(|rest| rest.strip_suffix(".md"))
            else {
                return Ok(None);
            };
            crate::validate::name(kind, name)?;
            Ok(Some(name.to_owned()))
        };
        if let Some(name) = named("modes/", "mode")? {
            return Ok(Self::Mode(name));
        }
        if let Some(name) = named("rules/", "rule")? {
            return Ok(Self::Rule(name));
        }
        bail!("a shared file is shared.toml, modes/<name>.md or rules/<name>.md, not {text}")
    }
}

impl TryFrom<String> for FileName {
    type Error = anyhow::Error;

    fn try_from(text: String) -> Result<Self> {
        text.parse()
    }
}

impl From<FileName> for String {
    fn from(name: FileName) -> Self {
        name.to_string()
    }
}

impl fmt::Display for FileName {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Config => f.write_str(Self::CONFIG),
            Self::Mode(name) => write!(f, "modes/{name}.md"),
            Self::Rule(name) => write!(f, "rules/{name}.md"),
        }
    }
}

/// A machine's edit of a shared file: its new text, none for a file it
/// removed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Edit {
    pub file: FileName,
    pub text: Option<String>,
}

/// The hub's copy of a shared file: the last edit of it that reached the
/// hub, at the revision the hub gave it, later edits of any file having
/// higher ones.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Copy {
    pub file: FileName,
    pub text: Option<String>,
    pub revision: i64,
}

#[cfg(test)]
mod tests;
