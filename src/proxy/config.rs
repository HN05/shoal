//! The global `[proxy.<name>]` tables and the proxy each AI tool's agents
//! reach their upstream through.
use std::{collections::BTreeMap, ffi::OsString};

use anyhow::{Context, Result, ensure};
use serde::Deserialize;

pub type Proxies = BTreeMap<String, Proxy>;

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Proxy {
    /// The `http` or `https` URL requests go to, the agent's own API base
    /// URL such as CLIProxyAPI's.
    pub upstream: String,
    /// The key the upstream gets instead of the agent's own credentials;
    /// without one, the agent's pass through.
    #[serde(default)]
    pub key: Option<Key>,
}

/// An upstream key, as written or from the agent's environment.
#[derive(Clone, PartialEq, Eq, Deserialize)]
#[serde(
    untagged,
    deny_unknown_fields,
    expecting = "a key, or { env = \"<variable>\" } naming the variable that holds it"
)]
pub enum Key {
    Raw(String),
    Env { env: String },
}

/// A key as written never shows in what Shoal prints.
impl std::fmt::Debug for Key {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Raw(_) => f.write_str("Raw(..)"),
            Self::Env { env } => f.debug_struct("Env").field("env", env).finish(),
        }
    }
}

impl Key {
    /// The key itself, reading `variable` of the agent's environment for
    /// one it names.
    pub fn value(&self, variable: impl Fn(&str) -> Option<OsString>) -> Result<String> {
        let name = match self {
            Self::Raw(key) => return Ok(key.clone()),
            Self::Env { env } => env,
        };
        let value = variable(name)
            .filter(|value| !value.is_empty())
            .with_context(|| format!("the proxy's key variable {name} is not set"))?;
        value
            .into_string()
            .map_err(|_| anyhow::anyhow!("the proxy's key variable {name} is not UTF-8"))
    }
}

pub fn validate(proxies: &Proxies) -> Result<()> {
    for (name, proxy) in proxies {
        crate::validate::name("proxy", name)?;
        validate_upstream(&proxy.upstream).with_context(|| format!("proxy.{name}.upstream"))?;
        match &proxy.key {
            Some(Key::Raw(key)) => ensure!(
                !key.is_empty() && key.bytes().all(|byte| byte.is_ascii_graphic()),
                "proxy.{name}.key must be visible ASCII text"
            ),
            Some(Key::Env { env }) => {
                crate::env::validate_configured_name(&format!("proxy.{name}.key"), env)?;
            }
            None => {}
        }
    }
    Ok(())
}

#[cfg(feature = "proxy")]
fn validate_upstream(url: &str) -> Result<()> {
    shoal_proxy::Upstream::parse(url).map(drop)
}

/// A build without the proxy runs no launch through one, which says so.
#[cfg(not(feature = "proxy"))]
fn validate_upstream(url: &str) -> Result<()> {
    ensure!(
        url.starts_with("http://") || url.starts_with("https://"),
        "upstream URL needs http:// or https://"
    );
    Ok(())
}

/// `ai.<name>.proxy` names a defined proxy.
pub fn validate_references(agents: &crate::ai::Agents, proxies: &Proxies) -> Result<()> {
    for (name, agent) in agents {
        if let Some(proxy) = &agent.proxy {
            ensure!(
                proxies.contains_key(proxy),
                "ai.{name}.proxy names {proxy:?}, which no [proxy.{proxy}] defines"
            );
        }
    }
    Ok(())
}

/// The proxy each AI tool's launches reach their upstream through.
pub fn routes(tools: &crate::ai::Tools, proxies: &Proxies) -> BTreeMap<String, Proxy> {
    tools
        .values()
        .filter_map(|tool| {
            let proxy = proxies.get(tool.proxy.as_ref()?)?.clone();
            Some((tool.name.clone(), proxy))
        })
        .collect()
}
