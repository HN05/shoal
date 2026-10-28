//! A proxied Claude Code's `--settings`: Claude Code takes only the last
//! one it is given, so a user's own would drop Shoal's hooks and proxy URL,
//! and Shoal's would drop the user's. They become one, which keeps both and
//! sends the API requests to the proxy.
use std::{ffi::OsString, path::Path};

use anyhow::{Context, Result};
use serde_json::{Map, Value};

/// Merge every `--settings` before a `--` in `args` into the first one's
/// place, in order, with `url` as its `ANTHROPIC_BASE_URL`. A value that is
/// not a JSON object names a settings file, relative to `directory`, where
/// the agent runs.
pub(super) fn merge(args: &mut Vec<OsString>, url: &str, directory: &Path) -> Result<()> {
    let found = occurrences(args);
    if found.is_empty() {
        return Ok(());
    }
    let mut merged = Value::Object(Map::new());
    for found in &found {
        merge_into(&mut merged, read(&found.value, directory)?);
    }
    set_base_url(&mut merged, url)?;
    replace(args, &found, &merged);
    Ok(())
}

/// Send `settings`' API requests to `url`, as its `env`'s
/// `ANTHROPIC_BASE_URL`; an error when that `env` is not an object.
fn set_base_url(settings: &mut Value, url: &str) -> Result<()> {
    let env = settings
        .as_object_mut()
        .expect("merged settings are an object")
        .entry("env")
        .or_insert_with(|| Value::Object(Map::new()));
    let Some(env) = env.as_object_mut() else {
        anyhow::bail!("Claude settings' env is not a JSON object");
    };
    env.insert("ANTHROPIC_BASE_URL".into(), Value::String(url.to_owned()));
    Ok(())
}

/// Put `merged` as one `--settings` in the place of the first of `found`,
/// and drop the rest.
fn replace(args: &mut Vec<OsString>, found: &[Occurrence], merged: &Value) {
    let Some(first) = found.first().map(|found| found.at.start) else {
        return;
    };
    for found in found.iter().rev() {
        args.drain(found.at.clone());
    }
    args.splice(
        first..first,
        ["--settings".into(), merged.to_string().into()],
    );
}

/// A `--settings` among the arguments, at the ones it takes up.
struct Occurrence {
    at: std::ops::Range<usize>,
    value: String,
}

/// Each `--settings VALUE` or `--settings=VALUE` before a `--`, in order.
fn occurrences(args: &[OsString]) -> Vec<Occurrence> {
    let mut found = Vec::new();
    let mut at = 0;
    while let Some(arg) = args.get(at) {
        let arg = arg.to_str().unwrap_or_default();
        if arg == "--" {
            break;
        }
        if arg == "--settings" {
            let value = args.get(at + 1).and_then(|value| value.to_str());
            if let Some(value) = value {
                found.push(Occurrence {
                    at: at..at + 2,
                    value: value.to_owned(),
                });
            }
            at += 2;
        } else if let Some(value) = arg.strip_prefix("--settings=") {
            found.push(Occurrence {
                at: at..at + 1,
                value: value.to_owned(),
            });
            at += 1;
        } else {
            at += 1;
        }
    }
    found
}

/// The settings `value` gives: JSON itself, or the file it names.
fn read(value: &str, directory: &Path) -> Result<Value> {
    let (text, source) = if value.trim_start().starts_with('{') {
        (value.to_owned(), "--settings".to_owned())
    } else {
        let path = directory.join(value);
        let text = std::fs::read_to_string(&path)
            .with_context(|| format!("read Claude settings {}", path.display()))?;
        (text, path.display().to_string())
    };
    let settings: Value =
        serde_json::from_str(&text).with_context(|| format!("parse Claude settings {source}"))?;
    anyhow::ensure!(
        settings.is_object(),
        "Claude settings {source} are not a JSON object"
    );
    Ok(settings)
}

/// Merge `next` over `merged`: objects key by key, arrays such as each
/// event's hooks one after the other, and anything else replaced.
fn merge_into(merged: &mut Value, next: Value) {
    match (merged, next) {
        (Value::Object(merged), Value::Object(next)) => {
            for (key, value) in next {
                merge_into(merged.entry(key).or_insert(Value::Null), value);
            }
        }
        (Value::Array(merged), Value::Array(next)) => merged.extend(next),
        (merged, next) => *merged = next,
    }
}

#[cfg(test)]
mod tests;
