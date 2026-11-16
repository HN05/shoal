//! A trigger's templates: text with `{{ path }}` lookups into a firing's
//! input document and nothing else, no conditionals, loops or functions.
use std::{fmt, str::FromStr};

use anyhow::{Result, bail, ensure};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::path::Path;

/// A template as config writes it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Template {
    text: String,
    parts: Vec<Part>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum Part {
    Text(String),
    Lookup(Path),
}

/// Why a template could not render: the path that reached no value.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Missing(pub Path);

impl fmt::Display for Missing {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{} has no value in the firing's input", self.0)
    }
}

impl std::error::Error for Missing {}

impl Template {
    /// The text as written.
    pub fn as_str(&self) -> &str {
        &self.text
    }

    /// Whether it looks anything up, so only a firing knows its value.
    pub fn is_static(&self) -> bool {
        !(self.parts.iter()).any(|part| matches!(part, Part::Lookup(_)))
    }

    /// The text with each lookup replaced by the value its path reaches in
    /// `input`: a string or number as text, an object or array as JSON.
    pub fn render(&self, input: &Value) -> Result<String, Missing> {
        let mut rendered = String::new();
        for part in &self.parts {
            match part {
                Part::Text(text) => rendered.push_str(text),
                Part::Lookup(path) => {
                    let value = path.value(input).ok_or_else(|| Missing(path.clone()))?;
                    match value {
                        Value::String(text) => rendered.push_str(text),
                        value => rendered.push_str(&value.to_string()),
                    }
                }
            }
        }
        Ok(rendered)
    }
}

impl FromStr for Template {
    type Err = anyhow::Error;

    fn from_str(text: &str) -> Result<Self> {
        let mut parts = Vec::new();
        let mut rest = text;
        while let Some(start) = rest.find("{{") {
            parts.extend(text_part(&rest[..start], text)?);
            let after = &rest[start + 2..];
            let Some(end) = after.find("}}") else {
                bail!(
                    "`{{{{` at byte {} has no closing `}}}}`",
                    text.len() - rest.len() + start
                );
            };
            let path: Path = after[..end].parse()?;
            ensure!(
                !path.fans_out(),
                "{path}: a template looks up one value, so its path has no []"
            );
            parts.push(Part::Lookup(path));
            rest = &after[end + 2..];
        }
        parts.extend(text_part(rest, text)?);
        Ok(Self {
            text: text.to_owned(),
            parts,
        })
    }
}

/// The text `between` lookups of `template`, none when empty, refusing a
/// `}}` that closes no `{{`.
fn text_part(between: &str, template: &str) -> Result<Option<Part>> {
    ensure!(
        !between.contains("}}"),
        "`}}}}` closes no `{{{{` in {template}"
    );
    Ok((!between.is_empty()).then(|| Part::Text(between.to_owned())))
}

impl fmt::Display for Template {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.text)
    }
}

impl Serialize for Template {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&self.text)
    }
}

impl<'de> Deserialize<'de> for Template {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let text = String::deserialize(deserializer)?;
        text.parse().map_err(serde::de::Error::custom)
    }
}

#[cfg(test)]
mod tests;
