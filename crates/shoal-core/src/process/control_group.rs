//! What a control group's memory files report on Linux's unified
//! hierarchy. A group that is gone, as a scope once its last process has,
//! reads as `None`.
use std::path::Path;

use anyhow::{Context, Result};

/// The bytes the control group `group` holds that reclaim cannot free,
/// with what it swapped out.
pub fn held(group: &Path) -> Result<Option<u64>> {
    let Some(current) = read(group, "memory.current")? else {
        return Ok(None);
    };
    let Some(stat) = read(group, "memory.stat")? else {
        return Ok(None);
    };
    // Without swap accounting the file is missing.
    let swap = read(group, "memory.swap.current")?;
    held_from(&current, &stat, swap.as_deref()).map(Some)
}

/// How many processes of the control group `group`, and of the groups
/// below it, any OOM killer killed.
pub fn oom_kills(group: &Path) -> Result<Option<u64>> {
    let Some(events) = read(group, "memory.events")? else {
        return Ok(None);
    };
    field(&events, "oom_kill").map(Some)
}

/// The file `name` of the control group `group`, `None` once it is gone.
fn read(group: &Path, name: &str) -> Result<Option<String>> {
    let path = group.join(name);
    match std::fs::read_to_string(&path) {
        Ok(text) => Ok(Some(text)),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error).with_context(|| format!("read {}", path.display())),
    }
}

/// The bytes a control group holds that reclaim cannot free, with what it
/// swapped out: its `memory.current`, `current`, less the page cache of
/// files `memory.stat`'s `stat` reports, but for shared memory, which only
/// swaps, and with its `memory.swap.current`, `swap`.
fn held_from(current: &str, stat: &str, swap: Option<&str>) -> Result<u64> {
    let cache = field(stat, "file")?.saturating_sub(field(stat, "shmem")?);
    let swapped = swap.map_or(Ok(0), number)?;
    Ok(number(current)?
        .saturating_sub(cache)
        .saturating_add(swapped))
}

/// The value of the key `name` in a flat keyed file's `text`.
fn field(text: &str, name: &str) -> Result<u64> {
    let value = (text.lines())
        .find_map(|line| line.strip_prefix(name)?.strip_prefix(' '))
        .with_context(|| format!("missing {name} in a control group's memory reading"))?;
    number(value)
}

fn number(text: &str) -> Result<u64> {
    (text.trim().parse()).with_context(|| format!("invalid memory reading {text:?}"))
}

#[cfg(test)]
mod tests;
