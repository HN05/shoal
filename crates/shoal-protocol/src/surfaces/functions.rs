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
    /// a value that is no call, or calls nested deeper than
    /// [`MAX_NESTED_CALLS`], which Shoal leaves to a renderer too.
    pub(super) fn called(&self, call: &Value, scope: &str) -> Option<Value> {
        if nesting(call) > MAX_NESTED_CALLS {
            return None;
        }
        match call.get("@call").and_then(Value::as_str)? {
            "@index" => self.indexed(call, scope),
            "formatString" => self.formatted(call, scope).map(Value::String),
            _ => None,
        }
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
