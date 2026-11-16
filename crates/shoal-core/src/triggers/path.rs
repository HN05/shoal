//! A path into a firing's input document, such as `payload.issue.number`,
//! `payload.labels.0.name` or `payload.labels[].name`: keys of objects and
//! indexes of arrays, separated by dots, where `[]` after a key reaches
//! every element of the array there.
use std::{fmt, str::FromStr};

use anyhow::{Result, bail, ensure};
use serde::{Deserialize, Serialize};
use serde_json::Value;

/// A path as config writes it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Path {
    text: String,
    steps: Vec<Step>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum Step {
    /// An object's key, or an array's index when it is a number.
    Key(String),
    /// Every element of an array.
    Each,
}

impl Path {
    /// Whether it reaches every element of an array, so it may name several
    /// values.
    pub fn fans_out(&self) -> bool {
        self.steps.contains(&Step::Each)
    }

    /// Its first key, a top-level key of the document.
    pub fn top(&self) -> &str {
        match self.steps.first() {
            Some(Step::Key(key)) => key,
            // A parsed path starts with a key.
            _ => "",
        }
    }

    /// Whether it names only a top-level key.
    pub fn is_top(&self) -> bool {
        self.steps.len() == 1
    }

    /// Every value it reaches in `document`; none where a step finds
    /// nothing, and `null` counts as nothing.
    pub fn values<'a>(&self, document: &'a Value) -> Vec<&'a Value> {
        let mut reached = vec![document];
        for step in &self.steps {
            reached = reached
                .into_iter()
                .flat_map(|value| step.apply(value))
                .collect();
        }
        reached.retain(|value| !value.is_null());
        reached
    }

    /// The one value a path without `[]` reaches in `document`.
    pub fn value<'a>(&self, document: &'a Value) -> Option<&'a Value> {
        self.values(document).into_iter().next()
    }
}

impl Step {
    fn apply<'a>(&self, value: &'a Value) -> Vec<&'a Value> {
        match (self, value) {
            (Self::Key(key), Value::Object(object)) => object.get(key).into_iter().collect(),
            (Self::Key(index), Value::Array(array)) => (index.parse::<usize>().ok())
                .and_then(|index| array.get(index))
                .into_iter()
                .collect(),
            (Self::Each, Value::Array(array)) => array.iter().collect(),
            _ => Vec::new(),
        }
    }
}

impl FromStr for Path {
    type Err = anyhow::Error;

    fn from_str(text: &str) -> Result<Self> {
        let text = text.trim();
        ensure!(!text.is_empty(), "a path names at least one key");
        let mut steps = Vec::new();
        for segment in text.split('.') {
            let (key, each) = match segment.strip_suffix("[]") {
                Some(key) => (key, true),
                None => (segment, false),
            };
            if key.is_empty()
                || key.contains(['[', ']', '{', '}'])
                || key.contains(char::is_whitespace)
            {
                bail!("{text}: `{segment}` is not a key, an index or a key followed by []");
            }
            steps.push(Step::Key(key.to_owned()));
            if each {
                steps.push(Step::Each);
            }
        }
        Ok(Self {
            text: text.to_owned(),
            steps,
        })
    }
}

impl fmt::Display for Path {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.text)
    }
}

impl Serialize for Path {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&self.text)
    }
}

impl<'de> Deserialize<'de> for Path {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let text = String::deserialize(deserializer)?;
        text.parse().map_err(serde::de::Error::custom)
    }
}
