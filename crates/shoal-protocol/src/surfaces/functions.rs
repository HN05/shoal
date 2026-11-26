//! The catalog functions Shoal runs itself where an answer's context calls
//! them: those whose result follows from the surface's data alone. A call
//! whose result depends on the renderer, such as on its locale or clock,
//! stays as written.
use serde_json::Value;

use super::{Surface, scoped};

impl Surface {
    /// A literal context `entry` of an action placed in `scope` as its
    /// event carries it: a call Shoal runs as [`Self::called`] gives it,
    /// and any other entry as written.
    pub(super) fn evaluated(&self, entry: &Value, scope: &str) -> Value {
        self.called(entry, scope).unwrap_or_else(|| entry.clone())
    }

    /// What `call`, placed in `scope`, gives where Shoal runs it, a call
    /// of `@index` or `formatString` as [`Self::indexed`] and
    /// [`Self::formatted`] give it; none for a call only a renderer runs,
    /// or a value that is no call.
    pub(super) fn called(&self, call: &Value, scope: &str) -> Option<Value> {
        match call.get("@call").and_then(Value::as_str)? {
            "@index" => self.indexed(call, scope),
            "formatString" => self.formatted(call, scope).map(Value::String),
            _ => None,
        }
    }

    /// The item's index a call of `@index` gives: the innermost list's
    /// items counted from 0, plus its `offset`, a number or data it binds
    /// in `scope`.
    fn indexed(&self, call: &Value, scope: &str) -> Option<Value> {
        let item = scope.rsplit('/').next()?.parse::<u32>().ok()?;
        let offset = (call.get("args").and_then(|args| args.get("offset")))
            .and_then(|offset| self.value(offset, scope))
            .map_or(Some(0.0), Value::as_f64)?;
        Some(number_value(f64::from(item) + offset))
    }

    /// The text a call of `formatString` gives: its `value`, literal or
    /// the data it binds in `scope`, with each `${path}` replaced by the
    /// data at that path, empty where there is none, and each `\${` a
    /// literal `${`. None where an expression calls a function, which only
    /// a renderer runs.
    fn formatted(&self, call: &Value, scope: &str) -> Option<String> {
        let template = self
            .value(call.get("args")?.get("value")?, scope)?
            .as_str()?;
        let mut text = String::new();
        let mut rest = template;
        while let Some(start) = rest.find("${") {
            if rest[..start].ends_with('\\') {
                text.push_str(&rest[..start - 1]);
                text.push_str("${");
                rest = &rest[start + 2..];
                continue;
            }
            text.push_str(&rest[..start]);
            let end = rest[start..].find('}')? + start;
            let path = rest[start + 2..end].trim();
            if path.contains('(') {
                return None;
            }
            text.push_str(
                &self
                    .data(&scoped(scope, path))
                    .map_or_else(String::new, shown),
            );
            rest = &rest[end + 1..];
        }
        text.push_str(rest);
        Some(text)
    }
}

/// `value` as interpolated text: a string as it is, null as nothing, and
/// anything else as JSON.
fn shown(value: &Value) -> String {
    match value {
        Value::String(text) => text.clone(),
        Value::Null => String::new(),
        other => other.to_string(),
    }
}

/// `number` as JSON: an integer when it is whole, as a renderer writes it.
fn number_value(number: f64) -> Value {
    if number.fract() == 0.0 && number.abs() < 1e15 {
        Value::from(number as i64)
    } else {
        serde_json::Number::from_f64(number).map_or(Value::Null, Value::Number)
    }
}
