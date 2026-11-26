//! A trigger's templates: text with `{{ path }}` lookups into a firing's
//! input document and nothing else, no conditionals, loops or functions.
use std::{fmt, str::FromStr};

use anyhow::{Result, bail, ensure};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::{
    input::{Input, Request},
    path::Path,
};

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
pub struct Missing(pub(crate) Path);

impl fmt::Display for Missing {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{} has no value in the firing's input", self.0)
    }
}

impl std::error::Error for Missing {}

impl Template {
    #[cfg(test)]
    pub(crate) fn as_str(&self) -> &str {
        &self.text
    }

    /// Whether it looks anything up, so only a firing knows its value.
    pub(crate) fn is_static(&self) -> bool {
        !(self.parts.iter()).any(|part| matches!(part, Part::Lookup(_)))
    }

    /// The text with each lookup replaced by the value its path reaches in
    /// `input`: a string or number as text, an object or array as JSON.
    pub fn render(&self, input: &Input) -> Result<String, Missing> {
        self.render_with(input, false)
    }

    /// [`Self::render`] with each value an outside request sent between
    /// the delimiters of its request, so that text reads as data.
    pub(crate) fn render_marked(&self, input: &Input) -> Result<String, Missing> {
        self.render_with(input, true)
    }

    /// The request whose data a lookup of it reads in `input`, if any.
    pub(crate) fn request_read<'a>(&self, input: &'a Input) -> Option<&'a Request> {
        (self.parts.iter()).find_map(|part| match part {
            Part::Lookup(path) => input.request_read_by(path),
            Part::Text(_) => None,
        })
    }

    fn render_with(&self, input: &Input, marked: bool) -> Result<String, Missing> {
        let mut rendered = String::new();
        for part in &self.parts {
            match part {
                Part::Text(text) => rendered.push_str(text),
                Part::Lookup(path) => {
                    let value = input.lookup(path).ok_or_else(|| Missing(path.clone()))?;
                    let text = match value.as_ref() {
                        Value::String(text) => text.clone(),
                        value => value.to_string(),
                    };
                    match input.request_read_by(path) {
                        Some(request) if marked => rendered.push_str(&request.mark(&text)),
                        _ => rendered.push_str(&text),
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
