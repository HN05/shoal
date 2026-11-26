//! The catalog functions Shoal runs itself where an answer's context calls
//! them: those whose result follows from the surface's data alone. A call
//! whose result depends on the renderer, such as on its locale or clock,
//! stays as written.
use serde_json::{Value, json};

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
    /// [`Self::formatted`] give it, a check's validation result, invalid
    /// where its value is known but of the wrong kind,
    /// such as text no number spells, and a logical function's boolean;
    /// none for a call only a renderer runs,
    /// a value that is no call, or calls nested deeper than
    /// [`MAX_NESTED_CALLS`], which Shoal leaves to a renderer too.
    pub(super) fn called(&self, call: &Value, scope: &str) -> Option<Value> {
        if nesting(call) > MAX_NESTED_CALLS {
            return None;
        }
        let arg = |name: &str| call.get("args")?.get(name);
        let given = |name: &str| self.known(arg(name)?, scope);
        let number = |name: &str| arg(name).and_then(Value::as_f64);
        let text = |name: &str| given(name).map(|value| value.as_str().map(str::to_owned));
        let check = |passes: Option<bool>| Some(validation(passes.unwrap_or(false)));
        match call.get("@call").and_then(Value::as_str)? {
            "@index" => self.indexed(call, scope),
            "formatString" => self.formatted(call, scope).map(Value::String),
            "required" => Some(validation(filled(&given("value")?))),
            "length" => {
                check(text("value")?.map(|value| {
                    within(value.chars().count() as f64, number("min"), number("max"))
                }))
            }
            "numeric" => check(
                numeric(&given("value")?).map(|value| within(value, number("min"), number("max"))),
            ),
            "email" => check(text("value")?.map(|value| email(&value))),
            "regex" => {
                let pattern = regex(arg("pattern")?.as_str()?)?;
                let value = text("value")?;
                match value {
                    Some(value) => Some(validation(pattern.is_match(&value).ok()?)),
                    None => check(None),
                }
            }
            "and" => self.logical(arg("values")?, scope, |values| {
                values.iter().all(|value| *value)
            }),
            "or" => self.logical(arg("values")?, scope, |values| {
                values.iter().any(|value| *value)
            }),
            "not" => Some(Value::Bool(!truth(&given("value")?)?)),
            _ => None,
        }
    }

    /// What the argument `value` gives in `scope` where Shoal can work it
    /// out: [`Self::dynamic`]'s value, with missing data as null; none for
    /// a call only a renderer runs.
    pub(super) fn known(&self, value: &Value, scope: &str) -> Option<Value> {
        match value.get("@call") {
            Some(_) => self.called(value, scope),
            None => Some(self.value(value, scope).cloned().unwrap_or(Value::Null)),
        }
    }

    /// What `combine` makes of the booleans `values` give in `scope`, each
    /// as [`truth`] reads it; none where one of them is none Shoal can work
    /// out.
    fn logical(&self, values: &Value, scope: &str, combine: fn(&[bool]) -> bool) -> Option<Value> {
        let values: Option<Vec<bool>> = (values.as_array()?.iter())
            .map(|value| truth(&self.known(value, scope)?))
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

/// The boolean `value` stands for, as a check's condition or a logical
/// function reads it: itself, a validation result's `valid`, or false for
/// null; none for anything else.
pub(super) fn truth(value: &Value) -> Option<bool> {
    match value {
        Value::Bool(value) => Some(*value),
        Value::Object(result) => result.get("valid")?.as_bool(),
        Value::Null => Some(false),
        _ => None,
    }
}

/// The validation result a check gives, as the catalog's check
/// functions return it.
fn validation(valid: bool) -> Value {
    json!({ "valid": valid })
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

/// `pattern` as a regular expression of the A2UI dialect, lookahead
/// included, none where it is longer than the dialect allows or Shoal
/// cannot compile it. A match that backtracks past [`MAX_BACKTRACKS`]
/// fails to run, which leaves the check to a renderer, as the dialect
/// rejects such a pattern.
fn regex(pattern: &str) -> Option<fancy_regex::Regex> {
    if pattern.chars().count() > MAX_PATTERN_CHARS {
        return None;
    }
    fancy_regex::RegexBuilder::new(&ecmascript(pattern))
        .backtrack_limit(MAX_BACKTRACKS)
        .build()
        .ok()
}

/// Most steps a regex check's match may backtrack.
const MAX_BACKTRACKS: usize = 100_000;

/// ECMAScript's whitespace and line terminators, which its `\s` matches,
/// as a character class's members.
const SPACE: &str = "\\t\\n\\x0B\\x0C\\r \\x{A0}\\x{1680}\\x{2000}-\\x{200A}\\x{2028}\\x{2029}\\x{202F}\\x{205F}\\x{3000}\\x{FEFF}";

/// An ASCII word character, as ECMAScript's `\w` is.
const WORD: &str = "[A-Za-z0-9_]";

/// `pattern`, written in ECMAScript's syntax, with what Rust's syntax
/// means otherwise spelled as ECMAScript means it: `\d`, `\w` and `\b`
/// ASCII, `\s` ECMAScript's whitespace, `.` excluding every line
/// terminator, and a character class as its literal members.
fn ecmascript(pattern: &str) -> String {
    let mut translated = String::new();
    let mut characters = pattern.chars().peekable();
    while let Some(character) = characters.next() {
        let spelled = match character {
            '\\' => escape(&mut characters, false),
            '[' => class(&mut characters),
            '.' => "[^\\n\\r\\x{2028}\\x{2029}]".to_owned(),
            other => other.to_string(),
        };
        translated.push_str(&spelled);
    }
    translated
}

/// The class whose `[` `characters` follow, through its `]`: `[]`, which
/// matches nothing, and `[^]`, which matches anything, as ECMAScript reads
/// them, and its members escaped where Rust would read `[`, `&&`, `--` or
/// `~~` as a nested class or a set operation.
fn class(characters: &mut Characters<'_>) -> String {
    let negated = characters.next_if_eq(&'^').is_some();
    if characters.next_if_eq(&']').is_some() {
        let matched = if negated {
            "[\\x{0}-\\x{10FFFF}]"
        } else {
            "[^\\x{0}-\\x{10FFFF}]"
        };
        return matched.to_owned();
    }
    let mut spelled = String::from(if negated { "[^" } else { "[" });
    let mut previous = None;
    while let Some(character) = characters.next() {
        match character {
            ']' => {
                spelled.push(']');
                return spelled;
            }
            '\\' => spelled.push_str(&escape(characters, true)),
            '[' | '&' | '~' => spelled.push_str(&format!("\\{character}")),
            '-' if previous == Some('-') => spelled.push_str("\\-"),
            other => spelled.push(other),
        }
        previous = Some(character);
    }
    spelled
}

/// The characters of a pattern, read one after another.
type Characters<'a> = std::iter::Peekable<std::str::Chars<'a>>;

/// What the escape whose `\\` `characters` follow means in ECMAScript,
/// spelled for Rust, `in_class` where a character class holds it: a class
/// as ECMAScript reads it, or a character as its code point.
fn escape(characters: &mut Characters<'_>, in_class: bool) -> String {
    let Some(escaped) = characters.next() else {
        return "\\\\".to_owned();
    };
    let code = |point: u32| format!("\\x{{{point:X}}}");
    match escaped {
        'd' => "[0-9]".to_owned(),
        'D' => "[^0-9]".to_owned(),
        'w' => WORD.to_owned(),
        'W' => "[^A-Za-z0-9_]".to_owned(),
        's' => format!("[{SPACE}]"),
        'S' => format!("[^{SPACE}]"),
        'b' if in_class => code(0x08),
        'b' => format!("(?:(?<={WORD})(?!{WORD})|(?<!{WORD})(?={WORD}))"),
        'B' => format!("(?:(?<={WORD})(?={WORD})|(?<!{WORD})(?!{WORD}))"),
        't' => code(0x09),
        'n' => code(0x0A),
        'v' => code(0x0B),
        'f' => code(0x0C),
        'r' => code(0x0D),
        '0' => code(0),
        'c' => match characters.next_if(char::is_ascii_alphabetic) {
            Some(letter) => code(u32::from(letter) % 32),
            None => "\\\\c".to_owned(),
        },
        'x' => hex(characters, 2).map_or_else(|| "x".to_owned(), code),
        'u' if characters.next_if_eq(&'{').is_some() => {
            let digits: String = std::iter::from_fn(|| characters.next_if(|c| *c != '}')).collect();
            characters.next_if_eq(&'}');
            u32::from_str_radix(&digits, 16).map_or_else(|_| format!("u{{{digits}}}"), code)
        }
        'u' => hex(characters, 4).map_or_else(|| "u".to_owned(), code),
        other => format!("\\{other}"),
    }
}

/// The number the next `digits` hex digits of `characters` spell, none
/// where fewer follow, which ECMAScript reads as the escaped letter.
fn hex(characters: &mut Characters<'_>, digits: usize) -> Option<u32> {
    let ahead: String = characters.clone().take(digits).collect();
    let number = (ahead.len() == digits && ahead.chars().all(|c| c.is_ascii_hexdigit()))
        .then(|| u32::from_str_radix(&ahead, 16).ok())
        .flatten()?;
    characters.nth(digits - 1);
    Some(number)
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
