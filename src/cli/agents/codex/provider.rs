//! The provider a Codex launch uses, from its config and the arguments it
//! runs with, and the overrides that send that provider to Shoal's proxy.
use std::{ffi::OsString, ops::Range, path::Path};

use anyhow::{Context, Result, bail};

/// Where a proxied Codex launch's hook arguments name its provider, which
/// [`route`] turns into overrides once the agent's environment says where
/// its config is.
pub(super) const PROVIDER: &str = "{shoal-proxy-codex-provider}";

/// Codex's built-in providers, which its config cannot override.
const BUILT_IN_PROVIDERS: [&str; 4] = ["openai", "oss", "ollama", "lmstudio"];

/// The provider Shoal defines for a Codex on its built-in OpenAI provider,
/// which uses the same login.
const PROXY_PROVIDER: &str = "shoal-proxy";

/// Turn the [`PROVIDER`] placeholder in `args` into the overrides that send
/// the provider the launch uses, by the Codex config at `config` and the
/// arguments themselves, to the proxy at `url`.
pub(super) fn route(args: &mut Vec<OsString>, url: &str, config: &Path) -> Result<()> {
    let Some(at) = args.iter().position(|arg| arg == PROVIDER) else {
        return Ok(());
    };
    let flags = Flags::of(args);
    let selected = Selected::of(read(config)?, &flags)?;
    let overrides = overrides(&selected, url)?;
    place_last(args, at, &flags, overrides);
    Ok(())
}

/// Replace the placeholder at `at` with `overrides`, after every override
/// the launch forwards: Codex applies them in order, so Shoal's win over a
/// forwarded provider, base URL or provider table.
fn place_last(args: &mut Vec<OsString>, at: usize, flags: &Flags, overrides: Vec<OsString>) {
    let after = flags
        .overrides
        .iter()
        .map(|config| config.at.end)
        .filter(|end| *end > at)
        .max()
        .unwrap_or(at + 1);
    args.splice(after..after, overrides);
    args.remove(at);
}

/// The provider a launch uses, and the config key that names it.
#[derive(Debug, PartialEq, Eq)]
struct Selected {
    provider: String,
    key: Vec<String>,
}

impl Selected {
    /// The provider `config`, with the arguments' overrides, selects: the
    /// selected profile's, else the top-level one, else `openai`.
    fn of(mut config: toml::Table, flags: &Flags) -> Result<Self> {
        if flags.oss {
            bail!(
                "Codex's built-in provider \"oss\", which --oss selects, cannot go through Shoal's proxy"
            );
        }
        for config_override in &flags.overrides {
            set(
                &mut config,
                &config_override.key,
                config_override.value.clone(),
            );
        }
        let profile = flags
            .profile
            .clone()
            .or_else(|| config.get("profile")?.as_str().map(str::to_owned));
        if let Some(profile) = profile {
            let provider = config
                .get("profiles")
                .and_then(|profiles| profiles.get(&profile))
                .and_then(|profile| profile.get("model_provider")?.as_str());
            if let Some(provider) = provider {
                return Ok(Self {
                    provider: provider.to_owned(),
                    key: vec!["profiles".into(), profile, "model_provider".into()],
                });
            }
        }
        Ok(Self {
            provider: config
                .get("model_provider")
                .and_then(toml::Value::as_str)
                .unwrap_or("openai")
                .to_owned(),
            key: vec!["model_provider".into()],
        })
    }
}

/// The overrides that send the `selected` provider to the proxy at `url`. A
/// provider of the user's keeps its own settings, such as a token, and only
/// changes its base URL; the built-in one, which Codex does not let config
/// change, gives way to Shoal's provider with the same OpenAI login, where
/// the launch named it. The proxy forwards plain HTTP.
fn overrides(selected: &Selected, url: &str) -> Result<Vec<OsString>> {
    let url = toml::Value::String(url.to_owned());
    let overrides = match selected.provider.as_str() {
        "openai" => vec![
            format!("{}=\"{PROXY_PROVIDER}\"", dotted(&selected.key)),
            format!(
                "model_providers.{PROXY_PROVIDER}={{name=\"Shoal proxy\",base_url={url},\
                 wire_api=\"responses\",requires_openai_auth=true,supports_websockets=false}}"
            ),
        ],
        id if BUILT_IN_PROVIDERS.contains(&id) => {
            bail!("Codex's built-in provider {id:?} cannot go through Shoal's proxy")
        }
        id => {
            let key = toml_edit::Key::new(id);
            vec![
                format!("model_providers.{key}.base_url={url}"),
                format!("model_providers.{key}.supports_websockets=false"),
            ]
        }
    };
    Ok(overrides
        .into_iter()
        .flat_map(|value| ["-c".into(), value.into()])
        .collect())
}

/// What Codex's arguments, before a `--`, say about its config.
#[derive(Debug, Default)]
struct Flags {
    /// Each `-c`/`--config` override, in order, so a later one wins.
    overrides: Vec<Override>,
    /// The profile `--profile`/`-p` selects.
    profile: Option<String>,
    /// Whether `--oss` selects the local open-source provider.
    oss: bool,
}

/// A `-c key=value` override, at the arguments it takes up.
#[derive(Debug)]
struct Override {
    at: Range<usize>,
    key: Vec<String>,
    value: toml::Value,
}

impl Flags {
    fn of(args: &[OsString]) -> Self {
        let mut flags = Self::default();
        let mut at = 0;
        while let Some(arg) = args.get(at) {
            let Some(arg) = arg.to_str() else {
                at += 1;
                continue;
            };
            if arg == "--" {
                break;
            }
            if arg == "--oss" {
                flags.oss = true;
            }
            if let Some((value, taken)) = option(args, at, "-c", "--config") {
                if let Some(config) = value.and_then(parse_override) {
                    let (key, value) = config;
                    flags.overrides.push(Override {
                        at: at..at + taken,
                        key,
                        value,
                    });
                }
                at += taken;
            } else if let Some((value, taken)) = option(args, at, "-p", "--profile") {
                flags.profile = value.map(str::to_owned).or(flags.profile);
                at += taken;
            } else {
                at += 1;
            }
        }
        flags
    }
}

/// The value of option `short` or `long` at `args[at]`, in any of the forms
/// Codex takes, with how many arguments it takes up; `None` for another
/// argument.
fn option<'a>(
    args: &'a [OsString],
    at: usize,
    short: &str,
    long: &str,
) -> Option<(Option<&'a str>, usize)> {
    let arg = args[at].to_str()?;
    if arg == short || arg == long {
        return Some((args.get(at + 1).and_then(|value| value.to_str()), 2));
    }
    if let Some(value) = arg
        .strip_prefix(long)
        .and_then(|rest| rest.strip_prefix('='))
    {
        return Some((Some(value), 1));
    }
    let value = arg.strip_prefix(short).filter(|rest| !rest.is_empty())?;
    Some((Some(value.strip_prefix('=').unwrap_or(value)), 1))
}

/// A `key=value` override as Codex reads it: a dotted key, and a TOML value
/// or, when the value is not one, the text itself.
fn parse_override(text: &str) -> Option<(Vec<String>, toml::Value)> {
    let (key, value) = text.split_once('=')?;
    let key = toml_edit::Key::parse(key.trim())
        .ok()?
        .into_iter()
        .map(|part| part.get().to_owned())
        .collect();
    let value = toml::from_str::<toml::Table>(&format!("value = {value}"))
        .ok()
        .and_then(|mut table| table.remove("value"))
        .unwrap_or_else(|| toml::Value::String(value.trim().to_owned()));
    Some((key, value))
}

/// Set dotted `key` of `table` to `value`, as an override does.
fn set(table: &mut toml::Table, key: &[String], value: toml::Value) {
    let Some((last, parents)) = key.split_last() else {
        return;
    };
    let mut table = table;
    for parent in parents {
        let entry = table
            .entry(parent.clone())
            .or_insert_with(|| toml::Value::Table(toml::Table::new()));
        if !entry.is_table() {
            *entry = toml::Value::Table(toml::Table::new());
        }
        table = entry.as_table_mut().expect("just made a table");
    }
    table.insert(last.clone(), value);
}

/// `key` as a dotted TOML key, quoting the parts that need it.
fn dotted(key: &[String]) -> String {
    key.iter()
        .map(|part| toml_edit::Key::new(part.as_str()).to_string())
        .collect::<Vec<_>>()
        .join(".")
}

/// The Codex config at `path`, empty when there is none.
fn read(path: &Path) -> Result<toml::Table> {
    let Some(text) = crate::fsutil::read_optional(path)? else {
        return Ok(toml::Table::new());
    };
    toml::from_str(&text).with_context(|| format!("parse {}", path.display()))
}

#[cfg(test)]
mod tests;
