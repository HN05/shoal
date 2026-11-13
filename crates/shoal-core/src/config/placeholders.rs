//! Single-pass substitution shared by text templates and command arguments.
use std::ffi::{OsStr, OsString};

pub fn render(template: &str, fields: &[(&str, &str)]) -> String {
    let mut output = String::new();
    render_into(template, fields, |value| output.push_str(value));
    output
}

pub fn render_os(template: &str, fields: &[(&str, &OsStr)]) -> OsString {
    let mut output = OsString::new();
    render_into(template, fields, |value| output.push(value));
    output
}

/// Keys include their braces. Scan only the template, never inserted values;
/// unknown fields and unmatched braces pass through unchanged.
fn render_into<V: ?Sized>(template: &str, fields: &[(&str, &V)], mut push: impl FnMut(&V))
where
    str: AsRef<V>,
{
    let mut rest = template;
    while let Some(start) = rest.find('{') {
        push(rest[..start].as_ref());
        rest = &rest[start..];
        if let Some((key, value)) = fields.iter().find(|(key, _)| rest.starts_with(key)) {
            push(value);
            rest = &rest[key.len()..];
        } else {
            push("{".as_ref());
            rest = &rest[1..];
        }
    }
    push(rest.as_ref());
}

#[cfg(test)]
mod tests;
