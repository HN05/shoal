//! How long Claude Code keeps its main conversation's prompt cache, from the
//! sources it reads, ranked as it ranks them.
use std::{ffi::OsString, path::Path, time::Duration};

use serde_json::Value;

const FIVE_MINUTES: Duration = Duration::from_secs(5 * 60);
const ONE_HOUR: Duration = Duration::from_secs(60 * 60);

/// Variables with which Claude Code uses another provider than a claude.ai
/// login, where it keeps the 5-minute cache unless told otherwise.
const OTHER_PROVIDERS: [&str; 3] = [
    "CLAUDE_CODE_USE_BEDROCK",
    "CLAUDE_CODE_USE_VERTEX",
    "CLAUDE_CODE_USE_FOUNDRY",
];

/// Variables whose credential Claude Code uses in place of a claude.ai
/// login.
const CREDENTIALS: [&str; 2] = ["ANTHROPIC_API_KEY", "ANTHROPIC_AUTH_TOKEN"];

/// The lifetime for a Claude Code that runs in the environment `variable`
/// reads, with its user settings under `home` unless `CLAUDE_CONFIG_DIR`
/// moves them. A claude.ai login past its usage limits keeps 5 minutes,
/// which Shoal cannot see, so it counts as within them.
pub(super) fn lifetime(variable: &dyn Fn(&str) -> Option<OsString>, home: &Path) -> Duration {
    let on = |name: &str| variable(name).is_some_and(|value| enabled(&value));
    let settings = user_settings(variable, home);
    if on("FORCE_PROMPT_CACHING_5M") {
        return FIVE_MINUTES;
    }
    let chosen = variable("CLAUDE_CODE_PROMPT_CACHE_TTL")
        .and_then(|value| parse(value.to_str()?))
        .or_else(|| parse(settings.get("promptCacheTtl")?.as_str()?));
    if let Some(lifetime) = chosen {
        return lifetime;
    }
    if on(super::PROMPT_CACHE_1H) {
        return ONE_HOUR;
    }
    let credential = CREDENTIALS
        .iter()
        .any(|name| variable(name).is_some_and(|value| !value.is_empty()));
    if credential
        || OTHER_PROVIDERS.iter().any(|name| on(name))
        || settings.get("apiKeyHelper").is_some()
    {
        FIVE_MINUTES
    } else {
        ONE_HOUR
    }
}

/// A lifetime as Claude Code names one.
fn parse(text: &str) -> Option<Duration> {
    match text {
        "5m" => Some(FIVE_MINUTES),
        "1h" => Some(ONE_HOUR),
        _ => None,
    }
}

/// Whether a flag variable is on: set to anything but a false value.
fn enabled(value: &OsString) -> bool {
    let value = value.to_string_lossy().to_ascii_lowercase();
    !matches!(value.as_str(), "" | "0" | "false" | "no" | "off")
}

/// The user's Claude settings, or nothing when they cannot be read.
fn user_settings(variable: &dyn Fn(&str) -> Option<OsString>, home: &Path) -> Value {
    let directory = variable(crate::env::CLAUDE_CONFIG_DIR)
        .map(std::path::PathBuf::from)
        .filter(|directory| directory.is_absolute())
        .unwrap_or_else(|| home.join(".claude"));
    std::fs::read(directory.join("settings.json"))
        .ok()
        .and_then(|bytes| serde_json::from_slice(&bytes).ok())
        .unwrap_or(Value::Null)
}

#[cfg(test)]
mod tests;
