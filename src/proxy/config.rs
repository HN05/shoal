//! The global `[proxy.<name>]` tables and the upstream each AI tool's
//! agents reach through one.
use std::collections::BTreeMap;

use anyhow::{Context, Result, ensure};
use serde::Deserialize;

pub type Proxies = BTreeMap<String, Proxy>;

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Proxy {
    /// The `http` or `https` URL requests go to, the agent's own API base
    /// URL such as CLIProxyAPI's.
    pub upstream: String,
}

pub fn validate(proxies: &Proxies) -> Result<()> {
    for (name, proxy) in proxies {
        crate::validate::name("proxy", name)?;
        validate_upstream(&proxy.upstream).with_context(|| format!("proxy.{name}.upstream"))?;
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

/// The upstream each AI tool's launches reach through a proxy.
pub fn upstreams(tools: &crate::ai::Tools, proxies: &Proxies) -> BTreeMap<String, String> {
    tools
        .values()
        .filter_map(|tool| {
            let upstream = proxies.get(tool.proxy.as_ref()?)?.upstream.clone();
            Some((tool.name.clone(), upstream))
        })
        .collect()
}
