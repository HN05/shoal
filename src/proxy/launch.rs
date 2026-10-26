//! The proxy a tracked agent's wrapper runs, the launch arguments that
//! name it, and the reports it sends the daemon.
#[cfg(feature = "proxy")]
use anyhow::Context;
use anyhow::Result;

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
                // The reset goes first, so the turn's retry waits past it.
                Event::Failed {
                    failure,
                    in_stream,
                    wait,
                } => {
                    self.retried(None).await;
                    if let Some(wait) = wait {
                        self.resets_after(wait).await;
                    }
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

    /// Tell the daemon the upstream refuses requests for `wait`, so a
    /// retry of the agent's turn, which the agent itself may report, waits
    /// past it.
    async fn resets_after(&self, wait: std::time::Duration) {
        let now = i64::try_from(crate::time::unix_seconds()).unwrap_or(i64::MAX);
        self.send(crate::protocol::AgentMethod::ReportUpstreamReset {
            workspace: self.workspace.clone(),
            execution: self.execution.clone(),
            resets_at: now.saturating_add(i64::try_from(wait.as_secs()).unwrap_or(i64::MAX)),
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

/// Start `proxy` on a free loopback port, serving on this process's
/// runtime, with its key from `variable` of the agent's environment when it
/// names one.
#[cfg(feature = "proxy")]
pub async fn start(
    proxy: &super::Proxy,
    variable: impl Fn(&str) -> Option<std::ffi::OsString>,
) -> Result<Running> {
    let mut upstream = shoal_proxy::Upstream::parse(&proxy.upstream)?;
    if let Some(key) = &proxy.key {
        upstream = upstream.with_key(&key.value(variable)?)?;
    }
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
pub async fn start(
    _proxy: &super::Proxy,
    _variable: impl Fn(&str) -> Option<std::ffi::OsString>,
) -> Result<Running> {
    anyhow::bail!(
        "this Shoal was built without the proxy feature, so it cannot start [proxy] tables"
    )
}
