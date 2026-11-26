//! A trigger's `filter`: conditions on a firing's input document, every one
//! of which must hold for the firing to go on. A path with `[]` holds when
//! any value it reaches does, and `not_equals` when none equals its value.
use std::fmt;

use anyhow::{Result, ensure};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::path::Path;

/// One condition, `{ path = "...", <operator> = ... }`, with exactly one
/// operator.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Condition {
    pub(crate) path: Path,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) equals: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) not_equals: Option<Value>,
    #[serde(default, rename = "in", skip_serializing_if = "Option::is_none")]
    pub(crate) any_of: Option<Vec<Value>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) starts_with: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) exists: Option<bool>,
}

impl Condition {
    /// Refuse a condition without exactly one operator.
    pub(crate) fn validate(&self) -> Result<()> {
        let operators = [
            self.equals.is_some(),
            self.not_equals.is_some(),
            self.any_of.is_some(),
            self.starts_with.is_some(),
            self.exists.is_some(),
        ];
        ensure!(
            operators.into_iter().filter(|set| *set).count() == 1,
            "the condition on {} needs exactly one of equals, not_equals, in, starts_with and exists",
            self.path
        );
        Ok(())
    }

    pub(crate) fn holds(&self, input: &Value) -> bool {
        let values = self.path.values(input);
        let any = |test: &dyn Fn(&Value) -> bool| values.iter().any(|value| test(value));
        if let Some(wanted) = &self.equals {
            return any(&|value| value == wanted);
        }
        if let Some(unwanted) = &self.not_equals {
            return !any(&|value| value == unwanted);
        }
        if let Some(wanted) = &self.any_of {
            return any(&|value| wanted.contains(value));
        }
        if let Some(prefix) = &self.starts_with {
            return any(&|value| value.as_str().is_some_and(|text| text.starts_with(prefix)));
        }
        self.exists
            .is_none_or(|exists| exists == !values.is_empty())
    }
}

/// The first condition of `filter` that does not hold for `input`, none
/// when every one does.
pub fn failing<'a>(filter: &'a [Condition], input: &Value) -> Option<&'a Condition> {
    filter.iter().find(|condition| !condition.holds(input))
}

impl fmt::Display for Condition {
    /// Its path, operator and value, the value as JSON.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let (operator, value) = match self {
            Self {
                equals: Some(value),
                ..
            } => ("equals", value.clone()),
            Self {
                not_equals: Some(value),
                ..
            } => ("not_equals", value.clone()),
            Self {
                any_of: Some(values),
                ..
            } => ("in", Value::Array(values.clone())),
            Self {
                starts_with: Some(prefix),
                ..
            } => ("starts_with", Value::String(prefix.clone())),
            Self { exists, .. } => ("exists", Value::Bool(exists.unwrap_or(true))),
        };
        write!(f, "{} {operator} {value}", self.path)
    }
}

#[cfg(test)]
mod tests;
