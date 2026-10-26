//! Shoal's retrying proxy between agents and their API upstream, from the
//! global `[proxy.<name>]` tables an `[ai.<name>]` tool names. The
//! `shoal-proxy` crate forwards and retries; Shoal configures it.
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

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(text: &str) -> Result<crate::config::Config> {
        let config: crate::config::Config = toml::from_str(text)?;
        validate(&config.proxy)?;
        validate_references(&config.ai.entries, &config.proxy)?;
        Ok(config)
    }

    #[test]
    fn tools_and_their_profiles_reach_the_upstream_of_the_proxy_they_name() {
        let config = parse(
            "[proxy.cliproxy]\nupstream = 'http://127.0.0.1:8317'\n\
             [ai.claude]\nproxy = 'cliproxy'\n[ai.claude-fast]\nbase = 'claude'\n",
        )
        .unwrap();
        let upstreams = upstreams(&config.ai.tools, &config.proxy);
        assert_eq!(upstreams["claude"], "http://127.0.0.1:8317");
        assert_eq!(upstreams["claude-fast"], "http://127.0.0.1:8317");
        assert!(!upstreams.contains_key("codex"));
        for (text, message) in [
            (
                "[ai.claude]\nproxy = 'missing'\n",
                "no [proxy.missing] defines",
            ),
            (
                "[proxy.local]\nupstream = 'ftp://host'\n",
                "proxy.local.upstream",
            ),
            ("[proxy.local]\nurl = 'http://host'\n", "unknown field"),
        ] {
            let error = format!("{:#}", parse(text).unwrap_err());
            assert!(error.contains(message), "{text}: {error}");
        }
    }
}
