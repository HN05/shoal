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
        let Some(upstream) = proxies.get(proxy).map(|proxy| &proxy.upstream) else {
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
