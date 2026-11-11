//! Claude Code's settings: the user's, and a proxied launch's `--settings`.
//! Claude Code takes only the last `--settings` it is given, so a user's own
//! would drop Shoal's hooks and proxy URL, and Shoal's would drop the
//! user's. They become one, which keeps both and sends the API requests to
//! the proxy.
use std::{ffi::OsString, path::Path};

use anyhow::{Context, Result};
use serde_json::{Map, Value};

/// Merge every `--settings` before a `--` in `args` into the first one's
/// place, in order, with the proxy's URL as its `ANTHROPIC_BASE_URL`. A value
/// that is not a JSON object names a settings file, relative to the
/// directory the agent runs in. The merge may hold what a private settings
/// file does, such as a token, so it goes in a file only the user can read
/// rather than on the command line, which any local user can.
pub(super) fn merge(args: &mut Vec<OsString>, routing: &super::super::Routing) -> Result<()> {
    let found = occurrences(args);
    if found.is_empty() {
        return Ok(());
    }
    let mut merged = join(&found, routing.directory)?;
    set_base_url(&mut merged, routing.url)?;
    let path = write_private(routing.private, &merged)?;
    replace(args, &found, path.as_os_str());
    Ok(())
}

/// What every `--settings` before a `--` in `args` gives, joined as
/// [`merge`] joins them, with files relative to `directory`.
pub(super) fn given(args: &[OsString], directory: &Path) -> Result<Value> {
    join(&occurrences(args), directory)
}

/// The settings of each of `found` merged in order, with files relative to
/// `directory`.
fn join(found: &[Occurrence], directory: &Path) -> Result<Value> {
    let mut merged = Value::Object(Map::new());
    for found in found {
        merge_into(&mut merged, read(&found.value, directory)?);
    }
    Ok(merged)
}

/// The user's Claude settings, under `home` unless `CLAUDE_CONFIG_DIR`,
/// which `variable` reads, moves them; nothing when they cannot be read.
pub(super) fn user(variable: &dyn Fn(&str) -> Option<OsString>, home: &Path) -> Value {
    let directory = variable(crate::env::CLAUDE_CONFIG_DIR)
        .map(std::path::PathBuf::from)
        .filter(|directory| directory.is_absolute())
        .unwrap_or_else(|| home.join(".claude"));
    read_file(&directory.join("settings.json"))
}

/// The settings of the project in `directory`, its local ones first, which
/// win over its shared ones; nothing for a file that cannot be read.
pub(super) fn project(directory: &Path) -> [Value; 2] {
    let settings = directory.join(".claude");
    ["settings.local.json", "settings.json"].map(|name| read_file(&settings.join(name)))
}

/// The settings in the file at `path`, or nothing when it cannot be read.
fn read_file(path: &Path) -> Value {
    std::fs::read(path)
        .ok()
        .and_then(|bytes| serde_json::from_slice(&bytes).ok())
        .unwrap_or(Value::Null)
}

/// Write `settings` to a file in `private` that only the user can read.
fn write_private(private: &Path, settings: &Value) -> Result<std::path::PathBuf> {
    use crate::fsutil::atomic::{Permissions, ReplaceOptions};
    let path = private.join("claude-settings.json");
    let options = ReplaceOptions {
        permissions: Permissions::Temporary,
        sync: false,
    };
    crate::fsutil::atomic::replace_atomically(&path, settings.to_string().as_bytes(), options)
        .with_context(|| format!("write {}", path.display()))?;
    Ok(path)
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
    env.insert(super::BASE_URL.into(), Value::String(url.to_owned()));
    Ok(())
}

/// Put `merged`, the path of the merged settings, as one `--settings` in
/// the place of the first of `found`, and drop the rest.
fn replace(args: &mut Vec<OsString>, found: &[Occurrence], merged: &std::ffi::OsStr) {
    let Some(first) = found.first().map(|found| found.at.start) else {
        return;
    };
    for found in found.iter().rev() {
        args.drain(found.at.clone());
    }
    args.splice(first..first, ["--settings".into(), merged.to_owned()]);
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
