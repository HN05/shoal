//! Herdr terminal handoffs; workspace ownership stays with the daemon.
use std::{ffi::OsString, path::Path};

use anyhow::{Context as _, Result, bail};
use serde::Deserialize;
use tokio::process::Command;

use futures_util::future::BoxFuture;

use super::{ChatStart, Demo, Host, Options, PlaceState, Start, Watched};

mod demo;
use crate::{
    config,
    sessions::{HostName, Origin},
    state::AgentState,
    subprocess::Run,
};

/// Herdr opens tabs beside the caller's pane and reads agent state from the
/// pane's process, so agents in its panes run in place.
pub(super) struct Herdr;

impl Host for Herdr {
    fn name(&self) -> HostName {
        HostName::Herdr
    }

    /// A Herdr pane.
    fn serves(&self) -> bool {
        in_herdr()
    }

    /// A tab in the Herdr workspace the start was queued from.
    fn takes_queued(&self, origin: Option<&Origin>) -> bool {
        origin.is_some_and(|origin| origin.host == HostName::Herdr)
    }

    fn start_add<'a>(&'a self, start: &'a Start<'a>) -> BoxFuture<'a, Result<bool>> {
        Box::pin(async move {
            open_tab(start).await?;
            Ok(true)
        })
    }

    /// A background tab.
    fn runs_in_background(&self) -> bool {
        true
    }

    fn run_in_background<'a>(&'a self, start: &'a Start<'a>) -> BoxFuture<'a, Result<()>> {
        Box::pin(open_tab(start))
    }

    /// A pane back at its shell prompt, as it is after the wrapper exits.
    fn run_in_place<'a>(
        &'a self,
        _state: &'a Path,
        place: &'a Origin,
        argv: &'a [OsString],
        env: &'a [(&'a str, String)],
    ) -> BoxFuture<'a, Result<bool>> {
        Box::pin(run_at_prompt(place, argv, env))
    }

    fn start_chat<'a>(&'a self, start: &'a Start<'a>) -> BoxFuture<'a, Result<ChatStart>> {
        Box::pin(async move {
            open_tab(start).await?;
            Ok(ChatStart::Tab)
        })
    }

    fn background_places(&self) -> &'static str {
        "Herdr tabs"
    }

    fn background_note(&self, name: &str) -> String {
        format!("Started {name} in a background Herdr tab")
    }

    fn origin(&self) -> Option<Origin> {
        origin()
    }

    fn place_variables(&self) -> &'static [&'static str] {
        &["HERDR_ENV", "HERDR_PANE_ID", "HERDR_TAB_ID"]
    }

    fn this_place(&self) -> Option<String> {
        current_tab()
    }

    fn close_place<'a>(&'a self, place: &'a str) -> BoxFuture<'a, Result<()>> {
        Box::pin(close_tab(place))
    }

    fn place_state<'a>(&'a self, place: &'a Watched) -> BoxFuture<'a, Result<PlaceState>> {
        Box::pin(tab_state(&place.id))
    }

    fn names_places(&self) -> bool {
        true
    }

    fn rename_place<'a>(&'a self, place: &'a str, label: &'a str) -> BoxFuture<'a, ()> {
        Box::pin(rename_tab(place, label))
    }

    fn start_demo<'a>(&'a self, demo: &'a Demo<'a>) -> BoxFuture<'a, Result<Option<Origin>>> {
        Box::pin(async move { demo::start(demo).await.map(Some) })
    }

    fn open_demo<'a>(&'a self, place: &'a Origin) -> BoxFuture<'a, Result<()>> {
        Box::pin(demo::open(place.id()))
    }

    fn demo_screen<'a>(&'a self, place: &'a Origin) -> BoxFuture<'a, Result<String>> {
        Box::pin(demo::screen(place.id()))
    }

    fn send_demo_keys<'a>(&'a self, place: &'a Origin, keys: &'a str) -> BoxFuture<'a, Result<()>> {
        Box::pin(demo::send(place.id(), keys))
    }
}

/// What Herdr's section sets: `herdr.enabled`, whether it opens tabs
/// (`herdr.new_tab`), their names (`herdr.tab_name`), and
/// `herdr.close_when_done`, which overrides the top-level option.
pub(super) fn options(config: &config::Herdr) -> Options {
    Options {
        enabled: config.enabled,
        opens_places: config.new_tab,
        name_template: config.tab_name.clone(),
        group_template: None,
        close_when_done: config.close_when_done,
    }
}

/// The pane this process runs in, in its Herdr workspace, with the
/// variables that reach Herdr's server, so a later start opens its tab in the
/// same workspace.
fn origin() -> Option<Origin> {
    in_herdr().then(|| Origin {
        host: HostName::Herdr,
        group: std::env::var(WORKSPACE_VAR)
            .ok()
            .filter(|id| !id.is_empty()),
        place: current_pane(),
        data: std::env::vars()
            .filter(|(name, _)| {
                name.starts_with("HERDR_")
                    && !matches!(
                        name.as_str(),
                        "HERDR_PANE_ID" | "HERDR_TAB_ID" | WORKSPACE_VAR
                    )
            })
            .collect(),
        owned: false,
    })
}

const WORKSPACE_VAR: &str = "HERDR_WORKSPACE_ID";

/// A `herdr` command reaching the server of `origin`.
fn herdr(origin: &Origin) -> Command {
    let mut command = Command::new("herdr");
    command.envs(&origin.data);
    command
}

/// Whether this process runs in Herdr, so new tabs can open beside it.
fn in_herdr() -> bool {
    std::env::var("HERDR_ENV").as_deref() == Ok("1")
}

/// Run `argv` in `pane` once it is back at its shell prompt. The pane's shell
/// keeps its own environment, so `env` goes along on the command.
async fn run_at_prompt(pane: &Origin, argv: &[OsString], env: &[(&str, String)]) -> Result<bool> {
    let Some(id) = pane.place.as_deref() else {
        return Ok(false);
    };
    if !at_prompt(pane, id).await {
        return Ok(false);
    }
    let mut command = vec!["env".into()];
    command.extend(
        env.iter()
            .map(|(name, value)| format!("{name}={value}").into()),
    );
    command.extend_from_slice(argv);
    run_in_pane(pane, id, &command).await?;
    Ok(true)
}

/// A pane that is gone, unreadable or running a program is not at its prompt.
async fn at_prompt(origin: &Origin, pane: &str) -> bool {
    let mut info = herdr(origin);
    info.args(["pane", "process-info", "--pane"]).arg(pane);
    let Ok(output) = Run::new(info).checked().await else {
        return false;
    };
    serde_json::from_slice::<ProcessInfo>(&output.stdout)
        .is_ok_and(|info| info.result.process_info.shell_in_foreground())
}

/// The Herdr pane this process runs in.
fn current_pane() -> Option<String> {
    if std::env::var("HERDR_ENV").as_deref() != Ok("1") {
        return None;
    }
    std::env::var("HERDR_PANE_ID")
        .ok()
        .filter(|pane| !pane.is_empty())
}

/// Run `start` in a new tab.
async fn open_tab(start: &Start<'_>) -> Result<()> {
    let origin = start.origin.context("Shoal does not run in a Herdr pane")?;
    let created = created(tab_command(
        origin,
        start.cwd,
        start.label,
        start.focus,
        start.env,
    )?)
    .await?;
    run_in_pane(origin, &created.root_pane.pane_id, start.argv).await
}

/// The command that creates a tab at `cwd` with `env` in the Herdr
/// workspace of `origin`. Herdr starts it in its server's environment, so
/// `env` carries what the tab needs from the caller's.
fn tab_command(
    origin: &Origin,
    cwd: &Path,
    label: &str,
    focus: bool,
    env: &[(&str, String)],
) -> Result<Command> {
    let workspace = origin
        .group
        .as_deref()
        .context("Herdr did not provide HERDR_WORKSPACE_ID")?;
    let mut create = herdr(origin);
    create
        .args(["tab", "create", "--workspace"])
        .arg(workspace)
        .arg("--cwd")
        .arg(cwd)
        .arg("--label")
        .arg(label)
        .arg(if focus { "--focus" } else { "--no-focus" });
    for (name, value) in env {
        create.arg("--env").arg(format!("{name}={value}"));
    }
    Ok(create)
}

async fn created(create: Command) -> Result<CreatedResult> {
    let output = Run::new(create).checked().await?;
    let created: Created =
        serde_json::from_slice(&output.stdout).context("invalid Herdr tab creation response")?;
    Ok(created.result)
}

async fn run_in_pane(origin: &Origin, pane: &str, argv: &[OsString]) -> Result<()> {
    let mut run = herdr(origin);
    run.args(["pane", "run"])
        .arg(pane)
        .arg(shell_command(argv)?);
    Run::new(run)
        .checked()
        .await
        .context("run Shoal in the Herdr tab (tab retained)")?;
    Ok(())
}

#[derive(Deserialize)]
struct ProcessInfo {
    result: ProcessInfoResult,
}
#[derive(Deserialize)]
struct ProcessInfoResult {
    process_info: PaneProcesses,
}
#[derive(Deserialize)]
struct PaneProcesses {
    foreground_process_group_id: Option<i64>,
    shell_pid: Option<i64>,
}

impl PaneProcesses {
    fn shell_in_foreground(&self) -> bool {
        self.shell_pid.is_some() && self.shell_pid == self.foreground_process_group_id
    }
}

#[derive(Deserialize)]
struct Created {
    result: CreatedResult,
}
#[derive(Deserialize)]
struct CreatedResult {
    root_pane: CreatedPane,
}
#[derive(Deserialize)]
struct CreatedPane {
    pane_id: String,
}

// Herdr's pane API sends shell text.
fn shell_command(argv: &[OsString]) -> Result<String> {
    let words = argv
        .iter()
        .map(|arg| arg.to_str().context("Herdr command path is not UTF-8"))
        .collect::<Result<Vec<_>>>()?;
    Ok(crate::shell::quote(&words))
}

async fn rename_tab(tab: &str, label: &str) {
    let mut rename = Command::new("herdr");
    rename.args(["tab", "rename"]).arg(tab).arg(label);
    if let Err(error) = Run::new(rename).checked().await {
        eprintln!("warning: cannot rename Herdr tab {tab}: {error:#}");
    }
}

async fn close_tab(tab: &str) -> Result<()> {
    let mut close = Command::new("herdr");
    close.args(["tab", "close"]).arg(tab);
    Run::new(close)
        .checked()
        .await
        .with_context(|| format!("cannot close Herdr tab {tab}"))?;
    Ok(())
}

/// The tab's agent state, or that Herdr reports the tab gone.
async fn tab_state(tab: &str) -> Result<PlaceState> {
    let mut get = Command::new("herdr");
    get.args(["tab", "get"]).arg(tab);
    let output = Run::new(get).capture().await.context("query Herdr tab")?;
    if output.status.success() {
        let status = serde_json::from_slice::<TabInfo>(&output.stdout)
            .map_or(AgentStatus::Unknown, |info| info.result.tab.agent_status);
        return Ok(PlaceState::Open(status.turn()));
    }
    if tab_not_found(&output.stdout) {
        return Ok(PlaceState::Gone);
    }
    bail!(
        "herdr tab get {tab} failed: {}",
        String::from_utf8_lossy(&output.stdout).trim()
    )
}

/// The Herdr tab this process runs in.
fn current_tab() -> Option<String> {
    std::env::var("HERDR_TAB_ID")
        .ok()
        .filter(|tab| !tab.is_empty())
}

#[derive(Deserialize)]
struct TabInfo {
    result: TabInfoResult,
}
#[derive(Deserialize)]
struct TabInfoResult {
    tab: TabState,
}
#[derive(Deserialize)]
struct TabState {
    #[serde(default)]
    agent_status: AgentStatus,
}

/// Herdr's answer to a failed query; a missing server is not a missing tab,
/// since a replacement server can keep its tabs.
#[derive(Deserialize)]
struct QueryFailure {
    error: QueryError,
}
#[derive(Deserialize)]
struct QueryError {
    code: String,
}

fn tab_not_found(stdout: &[u8]) -> bool {
    serde_json::from_slice::<QueryFailure>(stdout)
        .is_ok_and(|failure| failure.error.code == "tab_not_found")
}

/// Herdr's view of the agent in a tab; `Done` is a finished turn not yet seen.
#[derive(Clone, Copy, Debug, Default, Deserialize, PartialEq)]
#[serde(rename_all = "snake_case")]
enum AgentStatus {
    Idle,
    Working,
    Blocked,
    Done,
    #[default]
    #[serde(other)]
    Unknown,
}

impl AgentStatus {
    /// The turn state Shoal reads from Herdr's view.
    fn turn(self) -> Option<AgentState> {
        match self {
            Self::Idle | Self::Done => Some(AgentState::Idle),
            Self::Working => Some(AgentState::Working),
            Self::Blocked => Some(AgentState::Waiting),
            Self::Unknown => None,
        }
    }
}

#[cfg(test)]
mod tests;
