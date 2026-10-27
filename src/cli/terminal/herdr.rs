//! Herdr terminal handoffs; workspace ownership stays with the daemon.
use std::{collections::BTreeMap, ffi::OsString, path::Path};

use anyhow::{Context as _, Result, bail};
use serde::Deserialize;
use tokio::process::Command;

use futures_util::future::BoxFuture;

use super::{DemoWorker, Host, PlaceState, Placement};

mod demo;
use crate::cli::{commands::workspaces::AddPlan, context::Context, internal};
use crate::{
    config::{Config, Effective},
    env,
    execution::recovery::Record,
    model::{Repository, Workspace},
    state::AgentState,
    subprocess::Run,
};

/// Herdr opens tabs beside the caller's pane and reads agent state from the
/// pane's process, so agents in its panes run in place.
pub(super) struct Herdr;

impl Host for Herdr {
    fn name(&self) -> &'static str {
        "herdr"
    }

    /// A Herdr pane, unless `herdr.enabled` turns the integration off.
    fn serves(&self, _ctx: &Context, settings: &Effective) -> bool {
        in_herdr() && settings.herdr.enabled
    }

    /// A tab where Herdr may open one, from the Herdr variables the daemon
    /// gave the worker.
    fn takes_queued(&self, settings: &Effective) -> bool {
        in_herdr() && settings.herdr.enabled && settings.herdr.new_tab
    }

    fn start_add<'a>(
        &'a self,
        ctx: &'a Context,
        settings: &'a Effective,
        plan: &'a AddPlan,
        _name: &'a str,
        placement: Placement,
        worker: &'a [OsString],
    ) -> BoxFuture<'a, Result<bool>> {
        Box::pin(start_add(ctx, settings, plan, placement, worker))
    }

    /// A background tab, unless `herdr.new_tab` is off.
    fn runs_in_background(&self, settings: &Effective) -> bool {
        settings.herdr.new_tab
    }

    fn run_in_background<'a>(
        &'a self,
        ctx: &'a Context,
        workspace: &'a Workspace,
        label: &'a str,
        args: &'a [OsString],
    ) -> BoxFuture<'a, Result<()>> {
        Box::pin(run_in_tab(ctx, &workspace.path, label, args, false))
    }

    fn rerun_where_stopped<'a>(
        &'a self,
        ctx: &'a Context,
        record: &'a Path,
        args: &'a [OsString],
    ) -> BoxFuture<'a, Result<bool>> {
        Box::pin(rerun_in_recorded_pane(ctx, record, args))
    }

    fn start_chat<'a>(
        &'a self,
        ctx: &'a Context,
        settings: &'a Effective,
        repository: &'a Repository,
        _agent: &'a str,
        label: &'a str,
        args: &'a [OsString],
    ) -> BoxFuture<'a, Result<bool>> {
        Box::pin(async move {
            if !settings.herdr.new_tab {
                return Ok(false);
            }
            run_in_tab(ctx, &repository.path, label, args, true).await?;
            Ok(true)
        })
    }

    fn background_places(&self) -> &'static str {
        "Herdr tabs"
    }

    fn background_note(&self, name: &str) -> String {
        format!("Started {name} in a background Herdr tab")
    }

    fn environment(&self) -> BTreeMap<String, String> {
        environment()
    }

    fn current_place(&self) -> Option<String> {
        current_pane()
    }

    fn this_place(&self) -> Option<String> {
        current_tab()
    }

    fn close_place<'a>(&'a self, place: &'a str) -> BoxFuture<'a, Result<()>> {
        Box::pin(close_tab(place))
    }

    fn place_state<'a>(&'a self, place: &'a str) -> BoxFuture<'a, Result<PlaceState>> {
        Box::pin(tab_state(place))
    }

    fn closes_when_done(&self, settings: &Effective) -> bool {
        settings.herdr.close_when_done
    }

    fn names_places(&self) -> bool {
        true
    }

    fn name_template(&self, settings: &Effective) -> Option<String> {
        settings.herdr.tab_name.clone()
    }

    fn rename_place<'a>(&'a self, place: &'a str, label: &'a str) -> BoxFuture<'a, ()> {
        Box::pin(rename_tab(place, label))
    }

    fn start_demo<'a>(
        &'a self,
        ctx: &'a Context,
        workspace: &'a Workspace,
        name: &'a str,
        worker: &'a DemoWorker<'a>,
    ) -> BoxFuture<'a, Result<Option<String>>> {
        Box::pin(async move { demo::start(ctx, workspace, name, worker).await.map(Some) })
    }

    fn open_demo<'a>(&'a self, place: &'a str) -> BoxFuture<'a, Result<()>> {
        Box::pin(demo::open(place))
    }

    fn demo_screen<'a>(&'a self, place: &'a str) -> BoxFuture<'a, Result<String>> {
        Box::pin(demo::screen(place))
    }

    fn send_demo_keys<'a>(&'a self, place: &'a str, keys: &'a str) -> BoxFuture<'a, Result<()>> {
        Box::pin(demo::send(place, keys))
    }
}

/// Herdr's variables, so a later start opens its tab in the same Herdr
/// workspace; the launching pane and tab are left out.
fn environment() -> BTreeMap<String, String> {
    std::env::vars()
        .filter(|(name, _)| {
            name.starts_with("HERDR_") && !matches!(name.as_str(), "HERDR_PANE_ID" | "HERDR_TAB_ID")
        })
        .collect()
}

/// Whether this process runs in Herdr, so new tabs can open beside it.
fn in_herdr() -> bool {
    std::env::var("HERDR_ENV").as_deref() == Ok("1")
}

/// Run `plan` in a new tab with `worker`, taking focus as `placement` asks,
/// unless `herdr.new_tab` is off.
async fn start_add(
    ctx: &Context,
    settings: &Effective,
    plan: &AddPlan,
    placement: Placement,
    worker: &[OsString],
) -> Result<bool> {
    if !settings.herdr.new_tab {
        return Ok(false);
    }
    let directive = TabDirective::create()?;
    let created = create_tab(ctx, plan, &directive, placement == Placement::Focus).await?;
    run_in_pane(&created.root_pane.pane_id, worker).await?;
    directive.hand_over()?;
    Ok(true)
}

/// The file in which the worker leaves the workspace for the tab's shell to
/// enter. Shoal removes it until the tab runs its worker; then the shell
/// removes it at its next prompt, or the watcher when it closes the tab.
struct TabDirective(tempfile::TempPath);

impl TabDirective {
    fn create() -> Result<Self> {
        let file = tempfile::Builder::new()
            .prefix("shoal-cd.")
            .tempfile()
            .context("create the tab's shell directive")?;
        Ok(Self(file.into_temp_path()))
    }

    /// The tab environment entry naming the file.
    fn env(&self) -> String {
        format!("{}={}", env::SHELL_DIRECTIVE, self.0.display())
    }

    fn hand_over(self) -> Result<()> {
        self.0.keep().context("keep the tab's shell directive")?;
        Ok(())
    }
}

/// Run Shoal with `args` in the pane the agent of `record` stopped in, once
/// that pane is back at its shell prompt, as it is after the wrapper exits.
/// This terminal's own pane is never reused. Returns whether it ran.
async fn rerun_in_recorded_pane(ctx: &Context, record: &Path, args: &[OsString]) -> Result<bool> {
    let pane = std::fs::read(record)
        .ok()
        .and_then(|bytes| serde_json::from_slice::<Record>(&bytes).ok())
        .and_then(|record| record.place)
        .filter(|pane| current_pane().as_ref() != Some(pane));
    match pane {
        Some(pane) => run_at_prompt(ctx, &pane, args).await,
        None => Ok(false),
    }
}

async fn run_at_prompt(ctx: &Context, pane: &str, args: &[OsString]) -> Result<bool> {
    if !at_prompt(pane).await {
        return Ok(false);
    }
    // The pane's shell keeps its own environment, so the caller's output
    // setting goes along for the agent this starts.
    let (name, value) = env::output_setting();
    let mut argv = vec!["env".into(), format!("{name}={value}").into()];
    argv.extend(internal::shoal_argv(&ctx.paths)?);
    argv.extend_from_slice(args);
    run_in_pane(pane, &argv).await?;
    Ok(true)
}

/// A pane that is gone, unreadable or running a program is not at its prompt.
async fn at_prompt(pane: &str) -> bool {
    let mut info = Command::new("herdr");
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

/// Run Shoal with `args` in a new tab at `cwd`, focused when `focus` is set.
async fn run_in_tab(
    ctx: &Context,
    cwd: &Path,
    label: &str,
    args: &[OsString],
    focus: bool,
) -> Result<()> {
    let created = created(tab_command(ctx, cwd, label, focus)?).await?;
    let mut argv = internal::shoal_argv(&ctx.paths)?;
    argv.extend_from_slice(args);
    run_in_pane(&created.root_pane.pane_id, &argv).await
}

// The tab environment carries the plan, keeping the typed command short, and
// the file in which the worker leaves the workspace for the tab's shell to enter.
async fn create_tab(
    ctx: &Context,
    plan: &AddPlan,
    directive: &TabDirective,
    focus: bool,
) -> Result<CreatedResult> {
    let mut create = tab_command(ctx, &std::env::current_dir()?, &plan.label(), focus)?;
    create.arg("--env").arg(format!(
        "{}={}",
        env::ADD_PLAN,
        serde_json::to_string(plan)?
    ));
    create.arg("--env").arg(directive.env());
    created(create).await
}

fn tab_command(ctx: &Context, cwd: &Path, label: &str, focus: bool) -> Result<Command> {
    let workspace = std::env::var_os("HERDR_WORKSPACE_ID")
        .filter(|id| !id.is_empty())
        .context("Herdr did not provide HERDR_WORKSPACE_ID")?;
    let mut create = Command::new("herdr");
    create
        .args(["tab", "create", "--workspace"])
        .arg(workspace)
        .arg("--cwd")
        .arg(cwd)
        .arg("--label")
        .arg(label)
        .arg(if focus { "--focus" } else { "--no-focus" });
    // Preserve this CLI's config location, independently of the Herdr server's environment.
    let config = Config::path(&ctx.paths);
    let config_home = config
        .parent()
        .and_then(Path::parent)
        .context("config has no parent")?;
    create
        .arg("--env")
        .arg(format!("HOME={}", ctx.paths.home.display()));
    create
        .arg("--env")
        .arg(format!("XDG_CONFIG_HOME={}", config_home.display()));
    // Likewise its output setting, which agents the tab starts inherit.
    let (name, value) = crate::env::output_setting();
    create.arg("--env").arg(format!("{name}={value}"));
    Ok(create)
}

async fn created(create: Command) -> Result<CreatedResult> {
    let output = Run::new(create).checked().await?;
    let created: Created =
        serde_json::from_slice(&output.stdout).context("invalid Herdr tab creation response")?;
    Ok(created.result)
}

async fn run_in_pane(pane: &str, argv: &[OsString]) -> Result<()> {
    let mut run = Command::new("herdr");
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
