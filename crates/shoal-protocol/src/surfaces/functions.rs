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

    /// What `call`, placed in `scope`, gives where Shoal runs it: a call
    /// of `@index` or `formatString` as [`Self::indexed`] and
    /// [`Self::formatted`] give it, and a check's or logical function's
    /// boolean; none for a call only a renderer runs,
    /// a value that is no call, or calls nested deeper than
    /// [`MAX_NESTED_CALLS`], which Shoal leaves to a renderer too.
    pub(super) fn called(&self, call: &Value, scope: &str) -> Option<Value> {
        if nesting(call) > MAX_NESTED_CALLS {
            return None;
        }
        let arg = |name: &str| call.get("args")?.get(name);
        let given = |name: &str| self.dynamic(arg(name)?, scope);
        let number = |name: &str| arg(name).and_then(Value::as_f64);
        match call.get("@call").and_then(Value::as_str)? {
            "@index" => self.indexed(call, scope),
            "formatString" => self.formatted(call, scope).map(Value::String),
            "required" => Some(Value::Bool(
                given("value").is_some_and(|value| filled(&value)),
            )),
            "length" => {
                let length = given("value")?.as_str()?.chars().count() as f64;
                Some(Value::Bool(within(length, number("min"), number("max"))))
            }
            "numeric" => {
                let value = numeric(&given("value")?)?;
                Some(Value::Bool(within(value, number("min"), number("max"))))
            }
            "email" => Some(Value::Bool(email(given("value")?.as_str()?))),
            "regex" => {
                let pattern = regex(arg("pattern")?.as_str()?)?;
                Some(Value::Bool(pattern.is_match(given("value")?.as_str()?)))
            }
            "and" => self.logical(arg("values")?, scope, |values| {
                values.iter().all(|value| *value)
            }),
            "or" => self.logical(arg("values")?, scope, |values| {
                values.iter().any(|value| *value)
            }),
            "not" => Some(Value::Bool(!given("value")?.as_bool()?)),
            _ => None,
        }
    }

    /// What `combine` makes of the booleans `values` give in `scope`; none
    /// where one of them is no boolean Shoal can work out.
    fn logical(&self, values: &Value, scope: &str, combine: fn(&[bool]) -> bool) -> Option<Value> {
        let values: Option<Vec<bool>> = (values.as_array()?.iter())
            .map(|value| self.dynamic(value, scope)?.as_bool())
            .collect();
        Some(Value::Bool(combine(&values?)))
    }

    /// The item's index a call of `@index` gives: the innermost list's
    /// items counted from 0, plus its `offset`, a number, data it binds in
    /// `scope` or what a call Shoal runs there gives.
    fn indexed(&self, call: &Value, scope: &str) -> Option<Value> {
        let item = scope.rsplit('/').next()?.parse::<u32>().ok()?;
        let offset = (call.get("args").and_then(|args| args.get("offset")))
            .and_then(|offset| self.dynamic(offset, scope))
            .map_or(Some(0.0), |offset| offset.as_f64())?;
        Some(number_value(f64::from(item) + offset))
    }

    /// The text a call of `formatString` gives: its `value`, as
    /// [`Self::known_text`] gives it in `scope`, with each `${path}`
    /// replaced by the data at that path, empty where there is none, and
    /// each `\${` a literal `${`. None where an expression calls a
    /// function, which only a renderer runs, or where the text would
    /// outgrow [`MAX_FORMATTED_BYTES`].
    fn formatted(&self, call: &Value, scope: &str) -> Option<String> {
        let template = self.known_text(call.get("args")?.get("value")?, scope)?;
        let mut text = Bounded::default();
        let mut rest = template.as_str();
        while let Some(start) = rest.find("${") {
            if rest[..start].ends_with('\\') {
                text.push(&rest[..start - 1])?;
                text.push("${")?;
                rest = &rest[start + 2..];
                continue;
            }
            text.push(&rest[..start])?;
            let end = rest[start..].find('}')? + start;
            let path = rest[start + 2..end].trim();
            if path.contains('(') {
                return None;
            }
            match self.data(&scoped(scope, path)) {
                Some(Value::String(data)) => text.push(data)?,
                Some(Value::Null) | None => {}
                Some(other) => text.push(&other.to_string())?,
            }
            rest = &rest[end + 1..];
        }
        text.push(rest)?;
        Some(text.0)
    }
}

/// Whether `value` is set and not empty, as `required` checks.
fn filled(value: &Value) -> bool {
    match value {
        Value::Null => false,
        Value::String(text) => !text.is_empty(),
        Value::Array(items) => !items.is_empty(),
        Value::Object(fields) => !fields.is_empty(),
        Value::Bool(_) | Value::Number(_) => true,
    }
}

/// Whether `value` lies within `min` and `max`, each where given.
fn within(value: f64, min: Option<f64>, max: Option<f64>) -> bool {
    min.is_none_or(|min| value >= min) && max.is_none_or(|max| value <= max)
}

/// The number `value` is or spells.
fn numeric(value: &Value) -> Option<f64> {
    match value {
        Value::Number(number) => number.as_f64(),
        Value::String(text) => text.trim().parse().ok(),
        _ => None,
    }
}

/// Whether `text` is an email address: a local part and a domain of dotted
/// labels around one `@`, with no whitespace.
fn email(text: &str) -> bool {
    let Some((local, domain)) = text.split_once('@') else {
        return false;
    };
    !local.is_empty()
        && !domain.contains('@')
        && !text.chars().any(char::is_whitespace)
        && domain.contains('.')
        && domain.split('.').all(|label| !label.is_empty())
}

/// `pattern` as a regular expression of the A2UI dialect, none where it
/// is longer than the dialect allows or Shoal cannot compile it.
fn regex(pattern: &str) -> Option<regex::Regex> {
    if pattern.chars().count() > MAX_PATTERN_CHARS {
        return None;
    }
    regex::RegexBuilder::new(pattern)
        .size_limit(1 << 20)
        .build()
        .ok()
}

/// Most code points a `regex` check's pattern may hold, as the dialect
/// allows.
const MAX_PATTERN_CHARS: usize = 256;

/// Most calls Shoal runs nested in one another, such as a `formatString`
/// whose value is another. Each level of a template that interpolates
/// itself can double the text, so deeper ones are left to a renderer.
const MAX_NESTED_CALLS: usize = 4;

/// Most bytes a `formatString` Shoal runs gives, which keeps what a surface
/// can make it build near what a post's text may hold.
const MAX_FORMATTED_BYTES: usize = 8 * 1024;

/// Text built up to [`MAX_FORMATTED_BYTES`].
#[derive(Default)]
struct Bounded(String);

impl Bounded {
    /// Add `piece`, or none once the text would outgrow its bound.
    fn push(&mut self, piece: &str) -> Option<()> {
        (self.0.len() + piece.len() <= MAX_FORMATTED_BYTES).then(|| self.0.push_str(piece))
    }
}

/// How deeply calls nest in `value`: 0 for none, 1 for a call whose
/// arguments call nothing.
fn nesting(value: &Value) -> usize {
    let below = |values: &mut dyn Iterator<Item = &Value>| values.map(nesting).max().unwrap_or(0);
    match value {
        Value::Object(fields) => {
            usize::from(fields.contains_key("@call")) + below(&mut fields.values())
        }
        Value::Array(values) => below(&mut values.iter()),
        _ => 0,
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
