//! Shoal's retrying proxy between agents and their API upstream, from the
//! global `[proxy.<name>]` tables an `[ai.<name>]` tool names. The
//! `shoal-proxy` crate forwards and retries; Shoal configures and runs one
//! per agent launch.
use std::collections::BTreeMap;

use anyhow::{Context, Result, ensure};
use serde::Deserialize;

pub type Proxies = BTreeMap<String, Proxy>;

/// Where a proxied agent's launch arguments name the proxy's base URL,
/// which the wrapper fills in once the proxy listens.
pub const URL: &str = "{shoal-proxy-url}";

/// Fill in [`URL`] in each of `args` with the proxy's `url`.
pub fn fill_url(args: &mut [std::ffi::OsString], url: &str) {
    for arg in args {
        if let Some(text) = arg.to_str().filter(|text| text.contains(URL)) {
            *arg = text.replace(URL, url).into();
        }
    }
}

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

/// The proxy a tracked agent's wrapper runs while the agent does, at `url`.
pub struct Running {
    pub url: String,
    #[cfg(feature = "proxy")]
    serving: tokio::task::JoinHandle<std::io::Result<()>>,
    /// What the proxy reports, until [`Running::report_turns`] takes it.
    #[cfg(feature = "proxy")]
    events: Option<tokio::sync::mpsc::UnboundedReceiver<shoal_proxy::Event>>,
    #[cfg(feature = "proxy")]
    reporting: Option<tokio::task::JoinHandle<()>>,
}

impl Drop for Running {
    fn drop(&mut self) {
        #[cfg(feature = "proxy")]
        {
            self.serving.abort();
            if let Some(reporting) = &self.reporting {
                reporting.abort();
            }
        }
    }
}

impl Running {
    /// Tell the daemon about each turn the proxy ends for the agent that
    /// `execution` runs in `workspace`: a failure it gave up on inside a
    /// response stream ends the agent's turn, which the daemon may retry.
    pub fn report_turns(&mut self, paths: &crate::paths::Paths, workspace: &str, execution: &str) {
        #[cfg(feature = "proxy")]
        if let Some(events) = self.events.take() {
            let reporter = Reporter {
                paths: paths.clone(),
                workspace: workspace.to_owned(),
                execution: execution.to_owned(),
            };
            self.reporting = Some(tokio::spawn(reporter.run(events)));
        }
        #[cfg(not(feature = "proxy"))]
        let _ = (paths, workspace, execution);
    }
}

/// Reports a proxied agent's ended turns to the daemon.
#[cfg(feature = "proxy")]
struct Reporter {
    paths: crate::paths::Paths,
    workspace: String,
    execution: String,
}

#[cfg(feature = "proxy")]
impl Reporter {
    async fn run(self, mut events: tokio::sync::mpsc::UnboundedReceiver<shoal_proxy::Event>) {
        use shoal_proxy::Event;
        while let Some(event) = events.recv().await {
            match event {
                Event::Retrying {
                    failure,
                    attempt,
                    wait,
                } => self.retrying(turn_failure(failure), attempt, wait).await,
                Event::Failed { failure, in_stream } => {
                    self.retried(None).await;
                    if in_stream {
                        self.turn_failed(turn_failure(failure)).await;
                    }
                }
                Event::Recovered => self.retried(None).await,
            }
        }
    }

    /// Show the retry the proxy waits `wait` to make in the agent's status.
    async fn retrying(
        &self,
        failure: crate::state::TurnFailure,
        attempt: u32,
        wait: std::time::Duration,
    ) {
        let now = i64::try_from(crate::time::unix_seconds()).unwrap_or(i64::MAX);
        let retry = crate::daemon::workspace::proxy_retries::ProxyRetry {
            failure,
            attempt,
            next_at: now.saturating_add(i64::try_from(wait.as_secs()).unwrap_or(i64::MAX)),
        };
        self.retried(Some(retry)).await;
    }

    async fn retried(&self, retry: Option<crate::daemon::workspace::proxy_retries::ProxyRetry>) {
        self.send(crate::protocol::AgentMethod::ReportProxyRetry {
            workspace: self.workspace.clone(),
            execution: self.execution.clone(),
            retry,
        })
        .await;
    }

    async fn turn_failed(&self, failure: crate::state::TurnFailure) {
        self.send(crate::protocol::AgentMethod::ReportTurnFailure {
            workspace: self.workspace.clone(),
            failure,
            session: None,
            execution: Some(self.execution.clone()),
        })
        .await;
    }

    /// The wrapper writes nothing, since its terminal is the agent's: a
    /// report that fails leaves the retry or turn to the user.
    async fn send(&self, report: crate::protocol::AgentMethod) {
        let _ = crate::cli::client::call(&self.paths, report).await;
    }
}

/// The turn failure a proxy failure is.
#[cfg(feature = "proxy")]
fn turn_failure(failure: shoal_proxy::Failure) -> crate::state::TurnFailure {
    use crate::state::TurnFailure;
    use shoal_proxy::Failure;
    match failure {
        Failure::RateLimited => TurnFailure::RateLimited,
        Failure::Overloaded | Failure::Unreachable => TurnFailure::Overloaded,
        Failure::ServerError => TurnFailure::ServerError,
        Failure::Other => TurnFailure::Other,
    }
}

/// Start a proxy to `upstream` on a free loopback port, serving on this
/// process's runtime.
#[cfg(feature = "proxy")]
pub async fn start(upstream: &str) -> Result<Running> {
    let upstream = shoal_proxy::Upstream::parse(upstream)?;
    let (sender, events) = tokio::sync::mpsc::unbounded_channel();
    let proxy = shoal_proxy::Proxy::bind(upstream, sender)
        .await
        .context("start the agent's proxy")?;
    let url = format!("http://{}", proxy.local_addr()?);
    Ok(Running {
        url,
        serving: tokio::spawn(proxy.serve()),
        events: Some(events),
        reporting: None,
    })
}

#[cfg(not(feature = "proxy"))]
pub async fn start(_upstream: &str) -> Result<Running> {
    anyhow::bail!(
        "this Shoal was built without the proxy feature, so it cannot start [proxy] tables"
    )
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
    fn the_url_fills_in_wherever_launch_arguments_name_it() {
        let mut args: Vec<std::ffi::OsString> = vec![
            "--settings".into(),
            format!(r#"{{"env":{{"ANTHROPIC_BASE_URL":"{URL}"}}}}"#).into(),
            "plain".into(),
        ];
        fill_url(&mut args, "http://127.0.0.1:4000");
        assert_eq!(
            args[1],
            r#"{"env":{"ANTHROPIC_BASE_URL":"http://127.0.0.1:4000"}}"#
        );
        assert_eq!(args[2], "plain");
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
