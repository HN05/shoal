//! A trigger's `filter`: conditions on a firing's input document, every one
//! of which must hold for the firing to go on. A path with `[]` holds when
//! any value it reaches does, and `not_equals` when none equals its value.
use anyhow::{Result, ensure};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::path::Path;

/// One condition, `{ path = "...", <operator> = ... }`, with exactly one
/// operator.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Condition {
    pub path: Path,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub equals: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub not_equals: Option<Value>,
    #[serde(default, rename = "in", skip_serializing_if = "Option::is_none")]
    pub any_of: Option<Vec<Value>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub starts_with: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub exists: Option<bool>,
}

impl Condition {
    /// Refuse a condition without exactly one operator.
    pub fn validate(&self) -> Result<()> {
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

    /// Whether it holds for `input`.
    pub fn holds(&self, input: &Value) -> bool {
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

/// Whether every condition of `filter` holds for `input`.
pub fn passes(filter: &[Condition], input: &Value) -> bool {
    filter.iter().all(|condition| condition.holds(input))
}

#[cfg(test)]
mod tests;
