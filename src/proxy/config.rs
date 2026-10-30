//! The global `[machine.proxies.<name>]` tables and the proxy each AI tool's agents
//! reach their upstream through.
use std::collections::BTreeMap;

use anyhow::{Context, Result, ensure};
use serde::Deserialize;

use crate::config::value::Value;

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
    pub key: Option<Value>,
}

pub fn validate(proxies: &Proxies) -> Result<()> {
    for (name, proxy) in proxies {
        crate::validate::name("proxy", name)?;
        validate_upstream(&proxy.upstream)
            .with_context(|| format!("machine.proxies.{name}.upstream"))?;
        if let Some(key) = &proxy.key {
            key.validate(&format!("machine.proxies.{name}.key"))?;
        }
        if let Some(Value::Written(key)) = &proxy.key {
            ensure!(
                !key.is_empty() && key.bytes().all(|byte| byte.is_ascii_graphic()),
                "machine.proxies.{name}.key must be visible ASCII text"
            );
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
pub fn validate_references(
    agents: &BTreeMap<String, crate::config::agents::MachineAgent>,
    proxies: &Proxies,
) -> Result<()> {
    for (name, agent) in agents {
        if let Some(proxy) = &agent.proxy {
            ensure!(
                proxies.contains_key(proxy),
                "machine.agents.{name}.proxy names {proxy:?}, which no [machine.proxies.{proxy}] defines"
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
