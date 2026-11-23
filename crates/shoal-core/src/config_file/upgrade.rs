//! Rewriting what an earlier Shoal wrote into a global config file into the
//! shape this one reads, so each reader parses one shape: `[machine]
//! keep_awake` took a boolean, `true` meaning `"on"` and `false` `"off"`.
//! A file Shoal may not write, as a development build leaves the user's own
//! files, or whose edit lock is held, keeps what it says and fails to load
//! until a later read rewrites it; a reader that holds the lock rewrites it
//! first.
use std::path::Path;

use anyhow::{Context, Result};
use toml_edit::{DocumentMut, Value};

use crate::fsutil;

/// The global config file at `path`, whose text is `text`, in the shape
/// this Shoal reads, saved so when it was not; `text` when it cannot be
/// saved.
pub(crate) fn upgraded(path: &Path, text: String) -> String {
    if upgrade(&text).is_none() {
        return text;
    }
    save(path).unwrap_or(text)
}

/// Upgrade the file at `path` while holding its edit lock, so a concurrent
/// edit is kept, and return what it holds now; refused while another
/// holds the lock, which may be this reader's own caller.
fn save(path: &Path) -> Result<String> {
    let _lock = super::try_lock(path)?.context("the config is being edited")?;
    upgrade_held(path)
}

/// Upgrade the file at `path`, whose edit lock the caller holds, and
/// return what it holds now.
pub(crate) fn upgrade_held(path: &Path) -> Result<String> {
    let text = fsutil::read_optional(path)
        .with_context(|| format!("read {}", path.display()))?
        .unwrap_or_default();
    let Some(upgraded) = upgrade(&text) else {
        return Ok(text);
    };
    super::replace(path.to_owned(), &upgraded)?;
    Ok(upgraded)
}

/// `text` with an earlier Shoal's values rewritten, none when it has none
/// or is no TOML.
pub(crate) fn upgrade(text: &str) -> Option<String> {
    if !text.contains("keep_awake") {
        return None;
    }
    let mut document: DocumentMut = text.parse().ok()?;
    let value = (document.get_mut("machine")?.get_mut("keep_awake")?).as_value_mut()?;
    let level = match value.as_bool()? {
        true => "on",
        false => "off",
    };
    let decor = value.decor().clone();
    *value = Value::from(level);
    *value.decor_mut() = decor;
    Some(document.to_string())
}

#[cfg(test)]
mod tests;
