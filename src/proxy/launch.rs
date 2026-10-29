//! The proxy a tracked agent's wrapper runs, the key its requests name,
//! and the reports it sends the daemon.
#[cfg(feature = "proxy")]
use anyhow::Context;
use anyhow::Result;

/// The header an agent names its proxy's key in, which the proxy checks.
pub const KEY_HEADER: &str = "x-shoal-proxy-key";

/// The variable of a proxied agent's environment that holds its proxy's
/// key, for an agent that names a variable rather than a value: the
/// environment is the agent's alone, while any local user can read its
/// command line.
pub const KEY_VARIABLE: &str = "SHOAL_PROXY_KEY";

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
                sent: Sent::default(),
                now: crate::time::unix_seconds,
            };
            self.reporting = Some(tokio::spawn(reporter.run(events)));
        }
        #[cfg(not(feature = "proxy"))]
        let _ = (paths, workspace, execution, ends_turns);
    }
}

/// The binary an agent's wrapper started from, and whether an upgraded
/// daemon has refused its protocol, after which its reports go to the
/// installed release at once rather than to a call bound to fail.
pub(super) struct Origin {
    executable: crate::cli::installed::Executable,
    upgraded: std::sync::atomic::AtomicBool,
}

impl Origin {
    pub(super) fn at(executable: crate::cli::installed::Executable) -> Self {
        Self {
            executable,
            upgraded: std::sync::atomic::AtomicBool::new(false),
        }
    }
}

/// Send a wrapper's `report` to the daemon, through the installed release
/// when an upgrade changed the daemon's protocol under this wrapper;
/// whether the daemon took it.
pub async fn send_report(
    paths: &crate::paths::Paths,
    report: crate::protocol::AgentMethod,
) -> bool {
    send_from(paths, origin(), report).await
}

/// [`send_report`] from the wrapper `origin` describes.
pub(super) async fn send_from(
    paths: &crate::paths::Paths,
    origin: &Origin,
    report: crate::protocol::AgentMethod,
) -> bool {
    use std::sync::atomic::Ordering;
    let Ok(json) = serde_json::to_string(&report) else {
        return false;
    };
    if !origin.upgraded.load(Ordering::Relaxed) {
        match crate::cli::client::call(paths, report).await {
            Ok(_) => return true,
            Err(error) if error.is::<crate::cli::client::ProtocolMismatch>() => {
                origin.upgraded.store(true, Ordering::Relaxed);
            }
            Err(_) => return false,
        }
    }
    relay(paths, &origin.executable, &json).await
}

/// The wrapper's own [`Origin`], the binary it started from.
fn origin() -> &'static Origin {
    static ORIGIN: std::sync::OnceLock<Origin> = std::sync::OnceLock::new();
    ORIGIN.get_or_init(|| Origin::at(crate::cli::installed::started().clone()))
}

/// Hand `report`, a proxy report's JSON, to the binary installed over
/// `executable`, whose protocol the upgraded daemon speaks; whether it sent
/// it. Nothing runs when nothing replaced it.
async fn relay(
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

/// Send `report`, the JSON of a report a wrapper of an older release could
/// not, as the `proxy-report` worker does. Only the wrapper's own reports,
/// its proxy's and whether a terminal shows its agent, go through it.
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
                | AgentMethod::ReportAttached { .. }
        ),
        "the proxy-report worker sends only a wrapper's reports"
    );
    crate::cli::client::call(paths, report).await?;
    Ok(0)
}

/// Tell the daemon to forget what the proxy of `execution` in `workspace`
/// reported once another replaces it or none runs: the retry it waited to
/// make and its upstream's reset; whether the daemon took both. A report
/// that fails leaves them to the execution's end.
pub async fn forget_reports(paths: &crate::paths::Paths, workspace: &str, execution: &str) -> bool {
    use crate::protocol::AgentMethod;
    let retry = AgentMethod::ReportProxyRetry {
        workspace: workspace.to_owned(),
        execution: execution.to_owned(),
        retry: None,
    };
    let reset = AgentMethod::ReportUpstreamReset {
        workspace: workspace.to_owned(),
        execution: execution.to_owned(),
        resets_at: None,
    };
    let (retry, reset) = tokio::join!(send_report(paths, retry), send_report(paths, reset));
    retry && reset
}

/// Reports a proxied agent's ended turns to the daemon.
#[cfg(feature = "proxy")]
pub(super) struct Reporter {
    pub(super) paths: crate::paths::Paths,
    pub(super) workspace: String,
    pub(super) execution: String,
    /// Whether a failure stream ends the agent's turn.
    pub(super) ends_turns: bool,
    /// What the daemon last took, so nothing is cleared that was never
    /// shown and the same reset is not sent again; a report it did not take
    /// goes again with the next event.
    pub(super) sent: Sent,
    /// The Unix second now, which reset and retry times count from.
    pub(super) now: fn() -> u64,
}

/// What a [`Reporter`] last told the daemon.
#[cfg(feature = "proxy")]
#[derive(Default)]
pub(super) struct Sent {
    /// Whether it shows a retry the proxy waits to make.
    retry: bool,
    /// The Unix second until which the upstream refuses requests.
    reset: Option<i64>,
}

#[cfg(feature = "proxy")]
impl Reporter {
    pub(super) async fn run(
        mut self,
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
                    self.retry_done().await;
                    if let Some(wait) = wait {
                        self.reset(self.after(wait)).await;
                    }
                    if in_stream && self.ends_turns && !subagent {
                        self.turn_failed(turn_failure(failure)).await;
                    }
                }
                // An upstream that answers again refuses nothing until.
                Event::Recovered => self.recovered().await,
            }
        }
    }

    /// Show the retry the proxy waits `wait` to make in the agent's status.
    async fn retrying(
        &mut self,
        failure: crate::state::TurnFailure,
        attempt: u32,
        wait: std::time::Duration,
    ) {
        let retry = crate::daemon::workspace::proxy_retries::ProxyRetry {
            failure,
            attempt,
            next_at: self.after(wait),
        };
        self.sent.retry |= self.retried(Some(retry)).await;
    }

    /// Clear what the daemon holds of an outage that ended, each report on
    /// its own, keeping in [`Sent`] any clear it did not take, so the next
    /// event's reports go out as they should.
    async fn recovered(&mut self) {
        self.retry_done().await;
        if self.sent.reset.is_some() && self.reset_cleared().await {
            self.sent.reset = None;
        }
    }

    /// Whether the daemon took the clearing of the upstream's reset.
    async fn reset_cleared(&self) -> bool {
        self.send(crate::protocol::AgentMethod::ReportUpstreamReset {
            workspace: self.workspace.clone(),
            execution: self.execution.clone(),
            resets_at: None,
        })
        .await
    }

    /// Take the retry the agent's status shows away, when it shows one.
    async fn retry_done(&mut self) {
        if self.sent.retry && self.retried(None).await {
            self.sent.retry = false;
        }
    }

    async fn retried(
        &self,
        retry: Option<crate::daemon::workspace::proxy_retries::ProxyRetry>,
    ) -> bool {
        self.send(crate::protocol::AgentMethod::ReportProxyRetry {
            workspace: self.workspace.clone(),
            execution: self.execution.clone(),
            retry,
        })
        .await
    }

    /// The Unix second `wait` from now, saturating rather than wrapping.
    fn after(&self, wait: std::time::Duration) -> i64 {
        let now = i64::try_from((self.now)()).unwrap_or(i64::MAX);
        now.saturating_add(i64::try_from(wait.as_secs()).unwrap_or(i64::MAX))
    }

    /// Tell the daemon the upstream refuses requests until Unix second
    /// `resets_at`, so a retry of the agent's turn, which the agent itself
    /// may report, waits past it. The same reset, a second off for the time
    /// it took to arrive, is not sent again.
    async fn reset(&mut self, resets_at: i64) {
        if self
            .sent
            .reset
            .is_some_and(|sent| sent.abs_diff(resets_at) <= 1)
        {
            return;
        }
        let delivered = self
            .send(crate::protocol::AgentMethod::ReportUpstreamReset {
                workspace: self.workspace.clone(),
                execution: self.execution.clone(),
                resets_at: Some(resets_at),
            })
            .await;
        if delivered {
            self.sent.reset = Some(resets_at);
        }
    }

    async fn turn_failed(&self, failure: crate::state::TurnFailure) {
        self.send(crate::protocol::AgentMethod::ReportTurnFailure {
            workspace: self.workspace.clone(),
            failure,
            session: None,
            execution: Some(self.execution.clone()),
            turn_end: Default::default(),
        })
        .await;
    }

    /// The wrapper writes nothing, since its terminal is the agent's: a
    /// report that fails leaves the retry or turn to the user. One an
    /// upgraded daemon refuses for its protocol goes through the installed
    /// release instead.
    async fn send(&self, report: crate::protocol::AgentMethod) -> bool {
        send_report(&self.paths, report).await
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
