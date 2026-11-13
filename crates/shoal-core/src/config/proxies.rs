//! The global `[machine.proxies.<name>]` tables and the proxy each AI tool's agents
//! reach their upstream through.
use std::collections::BTreeMap;

use anyhow::{Context, Result, ensure};
use serde::{Deserialize, Serialize};

use crate::config::value::Value;

pub type Proxies = BTreeMap<String, Proxy>;

#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Proxy {
    /// The `http` or `https` server root requests go to, such as
    /// CLIProxyAPI's; without one, each agent's go where they would without
    /// the proxy.
    #[serde(default)]
    pub upstream: Option<String>,
    /// The key the upstream gets instead of the agent's own credentials;
    /// without one, the agent's pass through. Only an `upstream` takes one,
    /// so an agent's own API never gets a gateway's key.
    #[serde(default)]
    pub key: Option<Value>,
}

pub fn validate(proxies: &Proxies) -> Result<()> {
    for (name, proxy) in proxies {
        crate::validate::name("proxy", name)?;
        if let Some(upstream) = &proxy.upstream {
            validate_upstream(upstream)
                .with_context(|| format!("machine.proxies.{name}.upstream"))?;
        }
        ensure!(
            proxy.key.is_none() || proxy.upstream.is_some(),
            "machine.proxies.{name}.key needs an upstream"
        );
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

/// Only the scheme: the proxy checks the rest of the URL when it starts.
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

/// No Codex tool names a proxy whose `upstream` is Codex's own API URL
/// rather than a server root: Codex's requests reach the upstream under
/// `/v1`, so such an upstream would get it twice, or a path ChatGPT lacks.
pub fn validate_routes(tools: &crate::ai::Tools, proxies: &Proxies) -> Result<()> {
    let codex = tools
        .values()
        .filter(|tool| tool.integration == crate::ai::IntegrationKind::Codex);
    for proxy in codex.filter_map(|tool| tool.proxy.as_ref()) {
        let Some(upstream) = proxies.get(proxy).and_then(|proxy| proxy.upstream.as_ref()) else {
            continue;
        };
        ensure!(
            !is_codex_api(upstream),
            "machine.proxies.{proxy}.upstream {upstream:?} is Codex's API URL; change it to \
             the server root, which Codex reaches under /v1, or remove it"
        );
    }
    Ok(())
}

/// Whether `url` is an OpenAI-compatible API URL, which ends in `/v1`, or
/// ChatGPT's, rather than a server root.
fn is_codex_api(url: &str) -> bool {
    let rest = url.split_once("://").map_or(url, |(_, rest)| rest);
    let (authority, path) = rest.split_once('/').unwrap_or((rest, ""));
    let host = authority
        .rsplit_once('@')
        .map_or(authority, |(_, host)| host);
    let host = host.split_once(':').map_or(host, |(host, _)| host);
    let path = path.trim_end_matches('/');
    host.eq_ignore_ascii_case("chatgpt.com") || path == "v1" || path.ends_with("/v1")
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
