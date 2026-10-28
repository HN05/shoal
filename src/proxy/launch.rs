//! The proxy a tracked agent's wrapper runs, the launch arguments that
//! name it, and the reports it sends the daemon.
#[cfg(feature = "proxy")]
use anyhow::Context;
use anyhow::Result;

/// Where a proxied agent's launch arguments name the proxy's base URL,
/// which the wrapper fills in once the proxy listens.
pub const URL: &str = "{shoal-proxy-url}";

/// The header an agent names its proxy's key in, which the proxy checks.
pub const KEY_HEADER: &str = "x-shoal-proxy-key";

/// The variable of a proxied agent's environment that holds its proxy's
/// key, for an agent that names a variable rather than a value: the
/// environment is the agent's alone, while any local user can read its
/// command line.
pub const KEY_VARIABLE: &str = "SHOAL_PROXY_KEY";

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
    /// The key the agent names in [`KEY_HEADER`], which the proxy answers
    /// only with.
    pub key: String,
    /// A directory only the user can read, for what the agent's launch
    /// must keep off its command line; gone with the proxy.
    private: tempfile::TempDir,
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
    /// A directory only the user can read, which lasts as the proxy does.
    pub fn private_dir(&self) -> &std::path::Path {
        self.private.path()
    }

    /// Tell the daemon what the proxy learns for the agent that `execution`
    /// runs in `workspace`: the retries it waits to make and its upstream's
    /// resets, and, when `ends_turns`, each turn a failure it gave up on
    /// inside a response stream ends, which the daemon may retry. Only an
    /// agent that reports no failed turn itself takes such a failure for its
    /// turn's end, since a subagent's request may fail while the turn goes on.
    pub fn report_turns(
        &mut self,
        paths: &crate::paths::Paths,
        workspace: &str,
        execution: &str,
        ends_turns: bool,
    ) {
        #[cfg(feature = "proxy")]
        if let Some(events) = self.events.take() {
            let reporter = Reporter {
                paths: paths.clone(),
                workspace: workspace.to_owned(),
                execution: execution.to_owned(),
                ends_turns,
            };
            self.reporting = Some(tokio::spawn(reporter.run(events)));
        }
        #[cfg(not(feature = "proxy"))]
        let _ = (paths, workspace, execution, ends_turns);
    }
}

/// Send a wrapper's `report` to the daemon, through the installed release
/// when an upgrade changed the daemon's protocol under this wrapper.
async fn send(paths: &crate::paths::Paths, report: crate::protocol::AgentMethod) {
    send_with(paths, wrapper_executable(), report).await;
}

/// [`send`], with `executable` as the binary the wrapper started from.
pub(super) async fn send_with(
    paths: &crate::paths::Paths,
    executable: &crate::cli::installed::Executable,
    report: crate::protocol::AgentMethod,
) {
    let Ok(json) = serde_json::to_string(&report) else {
        return;
    };
    if let Err(error) = crate::cli::client::call(paths, report).await
        && error.is::<crate::cli::client::ProtocolMismatch>()
    {
        relay(paths, executable, &json).await;
    }
}

/// The binary this wrapper started from, which [`remember_executable`]
/// reads as it starts, before an upgrade can replace it.
fn wrapper_executable() -> &'static crate::cli::installed::Executable {
    static EXECUTABLE: std::sync::OnceLock<crate::cli::installed::Executable> =
        std::sync::OnceLock::new();
    EXECUTABLE.get_or_init(crate::cli::installed::Executable::current)
}

/// Read the binary an agent's wrapper started from, while it is still the
/// one at its path, so an upgrade later shows as its replacement and the
/// proxy's reports can go through the installed release. Once the binary
/// is replaced, the path no longer names the running one.
pub fn remember_executable() {
    wrapper_executable();
}

/// Hand `report`, a proxy report's JSON, to the binary installed over
/// `executable`, whose protocol the upgraded daemon speaks; whether it sent
/// it. Nothing runs when nothing replaced it.
pub(super) async fn relay(
    paths: &crate::paths::Paths,
    executable: &crate::cli::installed::Executable,
    report: &str,
) -> bool {
    let Some(installed) = executable.installed() else {
        return false;
    };
    let worker = crate::cli::internal::Worker::ProxyReport { report };
    let Ok(argv) = crate::cli::internal::internal_command(paths, true, worker) else {
        return false;
    };
    tokio::process::Command::new(installed)
        .args(&argv[1..])
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .await
        .is_ok_and(|status| status.success())
}

/// Send `report`, the JSON of a proxy report a wrapper of an older release
/// could not, as the `proxy-report` worker does. Only the wrapper's own
/// reports go through it.
pub async fn send_relayed(paths: &crate::paths::Paths, report: &str) -> Result<i32> {
    use crate::protocol::AgentMethod;
    let report: AgentMethod = serde_json::from_str(report).map_err(anyhow::Error::from)?;
    anyhow::ensure!(
        matches!(
            report,
            AgentMethod::ReportProxyRetry { .. }
                | AgentMethod::ReportUpstreamReset { .. }
                | AgentMethod::ReportTurnFailure {
                    execution: Some(_),
                    ..
                }
        ),
        "the proxy-report worker sends only a proxy's reports"
    );
    crate::cli::client::call(paths, report).await?;
    Ok(0)
}

/// Tell the daemon to forget what the proxy of `execution` in `workspace`
/// reported once another replaces it or none runs: the retry it waited to
/// make and its upstream's reset. A report that fails leaves them to the
/// execution's end.
pub async fn forget_reports(paths: &crate::paths::Paths, workspace: &str, execution: &str) {
    use crate::protocol::AgentMethod;
    let reports = [
        AgentMethod::ReportProxyRetry {
            workspace: workspace.to_owned(),
            execution: execution.to_owned(),
            retry: None,
        },
        AgentMethod::ReportUpstreamReset {
            workspace: workspace.to_owned(),
            execution: execution.to_owned(),
            resets_at: None,
        },
    ];
    for report in reports {
        send(paths, report).await;
    }
}

/// Reports a proxied agent's ended turns to the daemon.
#[cfg(feature = "proxy")]
pub(super) struct Reporter {
    pub(super) paths: crate::paths::Paths,
    pub(super) workspace: String,
    pub(super) execution: String,
    /// Whether a failure stream ends the agent's turn.
    pub(super) ends_turns: bool,
}

#[cfg(feature = "proxy")]
impl Reporter {
    pub(super) async fn run(
        self,
        mut events: tokio::sync::mpsc::UnboundedReceiver<shoal_proxy::Event>,
    ) {
        use shoal_proxy::Event;
        while let Some(event) = events.recv().await {
            match event {
                Event::Retrying {
                    failure,
                    attempt,
                    wait,
                } => self.retrying(turn_failure(failure), attempt, wait).await,
                // The reset goes first, so the turn's retry waits past it.
                // A subagent's failed stream ends no turn of the agent's own.
                Event::Failed {
                    failure,
                    in_stream,
                    wait,
                    subagent,
                } => {
                    self.retried(None).await;
                    if let Some(wait) = wait {
                        self.reset(Some(wait)).await;
                    }
                    if in_stream && self.ends_turns && !subagent {
                        self.turn_failed(turn_failure(failure)).await;
                    }
                }
                // An upstream that answers again refuses nothing until.
                Event::Recovered => {
                    self.retried(None).await;
                    self.reset(None).await;
                }
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
    /// past it; `None` once it answers again.
    async fn reset(&self, wait: Option<std::time::Duration>) {
        let now = i64::try_from(crate::time::unix_seconds()).unwrap_or(i64::MAX);
        let resets_at =
            wait.map(|wait| now.saturating_add(i64::try_from(wait.as_secs()).unwrap_or(i64::MAX)));
        self.send(crate::protocol::AgentMethod::ReportUpstreamReset {
            workspace: self.workspace.clone(),
            execution: self.execution.clone(),
            resets_at,
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
    /// report that fails leaves the retry or turn to the user. One an
    /// upgraded daemon refuses for its protocol goes through the installed
    /// release instead.
    async fn send(&self, report: crate::protocol::AgentMethod) {
        send(&self.paths, report).await;
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
        Failure::Exhausted | Failure::Other => TurnFailure::Other,
    }
}

/// Start `proxy` on a free loopback port, serving on this process's
/// runtime, sending the upstream `key`, the value of the proxy's own.
#[cfg(feature = "proxy")]
pub async fn start(proxy: &super::Proxy, key: Option<&str>) -> Result<Running> {
    let mut upstream = shoal_proxy::Upstream::parse(&proxy.upstream)?;
    if let Some(key) = key {
        upstream = upstream.with_key(key)?;
    }
    let (sender, events) = tokio::sync::mpsc::unbounded_channel();
    let key = uuid::Uuid::new_v4().simple().to_string();
    let proxy = shoal_proxy::Proxy::bind(upstream, &key, sender)
        .await
        .context("start the agent's proxy")?;
    let url = format!("http://{}", proxy.local_addr()?);
    let private = tempfile::Builder::new()
        .prefix("shoal-proxy-")
        .tempdir()
        .context("make the proxy's private directory")?;
    Ok(Running {
        url,
        key,
        private,
        serving: tokio::spawn(proxy.serve()),
        events: Some(events),
        reporting: None,
    })
}

#[cfg(not(feature = "proxy"))]
pub async fn start(_proxy: &super::Proxy, _key: Option<&str>) -> Result<Running> {
    anyhow::bail!(
        "this Shoal was built without the proxy feature, so it cannot start [proxy] tables"
    )
}
