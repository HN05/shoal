//! Herdr terminal handoffs; workspace ownership stays with the daemon.
use std::{collections::BTreeMap, ffi::OsString, path::Path, process::Stdio, time::Duration};

use anyhow::{Context as _, Result, bail};
use serde::{Deserialize, Serialize};
use tokio::process::Command;

use super::{
    client,
    commands::{
        issues::{self, Issue},
        workspaces::{AddPlan, execute_add},
    },
    context::Context,
    internal,
    terminal::{Place, Placement},
};
use crate::{
    config::{Config, Effective},
    env,
    execution::recovery::Record,
    model::{Repository, Workspace},
    subprocess::Run,
};

pub struct Tab {
    id: String,
    close_when_done: bool,
}

#[derive(Serialize, Deserialize)]
pub(in crate::cli) struct TabName {
    template: String,
    repo: String,
    issue_number: Option<u64>,
    issue_title: Option<String>,
}

impl TabName {
    fn new(template: String, repo: &Repository, issue: Option<&Issue>) -> Self {
        Self {
            template,
            repo: crate::forge::repository::name(repo).to_owned(),
            issue_number: issue.map(|issue| issue.number),
            issue_title: issue.map(|issue| issue.title.clone()),
        }
    }

    pub fn render(&self, branch: &str) -> String {
        crate::config::templates::render(
            &self.template,
            &[
                ("{repo}", &self.repo),
                ("{branch}", branch),
                (
                    "{issue_number}",
                    &self.issue_number.map(|n| n.to_string()).unwrap_or_default(),
                ),
                (
                    "{issue_title}",
                    self.issue_title.as_deref().unwrap_or_default(),
                ),
            ],
        )
    }
}

/// Herdr's variables, so a later start opens its tab in the same Herdr
/// workspace; the launching pane and tab are left out.
pub(in crate::cli) fn environment() -> BTreeMap<String, String> {
    std::env::vars()
        .filter(|(name, _)| {
            name.starts_with("HERDR_") && !matches!(name.as_str(), "HERDR_PANE_ID" | "HERDR_TAB_ID")
        })
        .collect()
}

/// Whether this process runs in Herdr, so new tabs can open beside it.
pub(in crate::cli) fn in_herdr() -> bool {
    std::env::var("HERDR_ENV").as_deref() == Ok("1")
}

/// Run `plan` in a new tab that takes focus as `placement` asks, unless
/// `herdr.new_tab` is off. Without `issue`, a tab name template that names it
/// looks it up.
pub(in crate::cli) async fn start_add(
    ctx: &Context,
    settings: &Effective,
    plan: &mut AddPlan,
    issue: Option<&Issue>,
    placement: Placement,
) -> Result<bool> {
    if !settings.herdr.new_tab {
        return Ok(false);
    }
    if let Some(template) = settings.herdr.tab_name.clone() {
        let repos = client::repositories(&ctx.paths).await?;
        let repo = crate::forge::repository::select(&repos, &plan.repository).await?;
        // A queued start has only the issue's URL; the template may name it.
        let found = match (issue, plan.issue_url()) {
            (None, Some(url)) => Some(issues::load(&ctx.forge_account()?, repo, url).await?),
            _ => None,
        };
        plan.tab_name = Some(TabName::new(template, repo, issue.or(found.as_ref())));
    }
    let created = create_tab(ctx, plan, placement == Placement::Focus).await?;
    submit_worker(ctx, created, settings.herdr.close_when_done).await?;
    Ok(true)
}

/// The label `shoal add` gives a workspace's tab: the `tab_name` template,
/// `<repo>#<number>` for an issue, or the branch.
pub(in crate::cli) fn tab_label(
    template: Option<String>,
    repo: &Repository,
    issue: Option<&Issue>,
    branch: &str,
) -> String {
    match template {
        Some(template) => TabName::new(template, repo, issue).render(branch),
        None => issue.map_or_else(|| branch.to_owned(), |issue| issue.tab_label(repo)),
    }
}

/// Run Shoal with `args` in the pane the agent of `record` stopped in, once
/// that pane is back at its shell prompt, as it is after the wrapper exits.
/// This terminal's own pane is never reused. Returns whether it ran.
pub(in crate::cli) async fn rerun_in_recorded_pane(
    ctx: &Context,
    record: &Path,
    args: &[OsString],
) -> Result<bool> {
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
    let mut argv = internal::shoal_argv(&ctx.paths)?;
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
pub(in crate::cli) fn current_pane() -> Option<String> {
    if std::env::var("HERDR_ENV").as_deref() != Ok("1") {
        return None;
    }
    std::env::var("HERDR_PANE_ID")
        .ok()
        .filter(|pane| !pane.is_empty())
}

/// Whether this terminal can hand Shoal work to new Herdr tabs.
pub(in crate::cli) fn available(ctx: &Context) -> bool {
    ctx.place.is_none() && ctx.interactive() && in_herdr()
}

/// Run Shoal with `args` in a new tab at `cwd`, focused when `focus` is set.
pub(in crate::cli) async fn run_in_tab(
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

/// Run Shoal with `args` in a new background tab in `workspace`, labelled
/// as `shoal add` labels it, unless `herdr.new_tab` is off.
pub(in crate::cli) async fn run_in_background(
    ctx: &Context,
    settings: &Effective,
    workspace: &Workspace,
    args: &[OsString],
) -> Result<bool> {
    if !settings.herdr.new_tab {
        return Ok(false);
    }
    let label = workspace_label(ctx, settings, workspace).await;
    run_in_tab(ctx, &workspace.path, &label, args, false).await?;
    Ok(true)
}

/// The tab label `shoal add` would give `workspace`. A failed issue lookup
/// falls back to the branch.
async fn workspace_label(ctx: &Context, settings: &Effective, workspace: &Workspace) -> String {
    let labelled = async {
        let repositories = client::repositories(&ctx.paths).await?;
        let repo = repositories
            .iter()
            .find(|repo| repo.id == workspace.repository_id)
            .context("repository is no longer registered")?;
        let issue = match &workspace.links.issue {
            Some(url) => Some(issues::load(&ctx.forge_account()?, repo, url).await?),
            None => None,
        };
        anyhow::Ok(tab_label(
            settings.herdr.tab_name.clone(),
            repo,
            issue.as_ref(),
            &workspace.branch,
        ))
    };
    labelled.await.unwrap_or_else(|error| {
        eprintln!("warning: cannot label the Herdr tab: {error:#}");
        workspace.branch.clone()
    })
}

// The tab environment carries the plan, keeping the typed command short, and
// the file in which the worker leaves the workspace for the tab's shell to enter.
async fn create_tab(ctx: &Context, plan: &AddPlan, focus: bool) -> Result<CreatedResult> {
    let mut create = tab_command(ctx, &std::env::current_dir()?, &plan.label(), focus)?;
    create.arg("--env").arg(format!(
        "{}={}",
        env::ADD_PLAN,
        serde_json::to_string(plan)?
    ));
    let directive = tempfile::Builder::new()
        .prefix("shoal-cd.")
        .tempfile()
        .context("create the tab's shell directive")?
        .into_temp_path()
        .keep()
        .context("keep the tab's shell directive")?;
    create
        .arg("--env")
        .arg(format!("{}={}", env::SHELL_DIRECTIVE, directive.display()));
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
    Ok(create)
}

async fn created(create: Command) -> Result<CreatedResult> {
    let output = Run::new(create).checked().await?;
    let created: Created =
        serde_json::from_slice(&output.stdout).context("invalid Herdr tab creation response")?;
    Ok(created.result)
}

async fn submit_worker(ctx: &Context, created: CreatedResult, close_when_done: bool) -> Result<()> {
    let mut argv = internal::shoal_argv(&ctx.paths)?;
    argv.extend([internal::GROUP.into(), internal::HERDR.into()]);
    if close_when_done {
        argv.push("--close-when-done".into());
    }
    run_in_pane(&created.root_pane.pane_id, &argv).await
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

pub async fn worker(mut ctx: Context, close_when_done: bool, payload: &str) -> Result<i32> {
    let plan = serde_json::from_str(payload).context("invalid Herdr workspace plan")?;
    let id = std::env::var("HERDR_TAB_ID")
        .ok()
        .filter(|id| !id.is_empty())
        .context("Herdr did not provide HERDR_TAB_ID")?;
    ctx.place = Some(Place::Tab(Tab {
        id,
        close_when_done,
    }));
    execute_add(&ctx, plan).await
}

impl Tab {
    pub async fn rename(&self, branch: &str) {
        let mut rename = Command::new("herdr");
        rename.args(["tab", "rename"]).arg(&self.id).arg(branch);
        if let Err(error) = Run::new(rename).checked().await {
            eprintln!("warning: cannot rename Herdr tab {}: {error:#}", self.id);
        }
    }

    pub async fn close(&self) {
        if !self.close_when_done {
            return;
        }
        let mut close = Command::new("herdr");
        close.args(["tab", "close"]).arg(&self.id);
        if let Err(error) = Run::new(close).checked().await {
            eprintln!("warning: cannot close Herdr tab {}: {error:#}", self.id);
        }
    }

    /// The tab's agent state; `None` once Herdr reports the tab gone.
    async fn agent_status(&self) -> Result<Option<AgentStatus>> {
        let mut get = Command::new("herdr");
        get.args(["tab", "get"]).arg(&self.id);
        let output = Run::new(get).capture().await.context("query Herdr tab")?;
        if output.status.success() {
            return Ok(Some(
                serde_json::from_slice::<TabInfo>(&output.stdout)
                    .map_or(AgentStatus::Unknown, |info| info.result.tab.agent_status),
            ));
        }
        if tab_not_found(&output.stdout) {
            return Ok(None);
        }
        bail!(
            "herdr tab get {} failed: {}",
            self.id,
            String::from_utf8_lossy(&output.stdout).trim()
        )
    }

    /// Observe lifecycle separately so returning from an agent leaves the tab open.
    pub fn watch_workspace(&self, ctx: &Context, workspace: &str) -> Result<()> {
        if !self.close_when_done {
            return Ok(());
        }
        let argv = internal::internal_command(
            &ctx.paths,
            ctx.json,
            internal::Worker::HerdrWatch {
                workspace,
                tab: &self.id,
            },
        )?;
        let mut watcher = Command::new(&argv[0]);
        watcher
            .args(&argv[1..])
            .current_dir(&ctx.paths.home)
            .env_remove(env::SHELL_DIRECTIVE)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        // SAFETY: setsid detaches the forked child without allocating, before exec.
        unsafe {
            watcher.pre_exec(|| {
                if libc::setsid() == -1 {
                    return Err(std::io::Error::last_os_error());
                }
                Ok(())
            });
        }
        watcher
            .spawn()
            .context("watch workspace for Herdr tab closure")?;
        Ok(())
    }
}

pub async fn watch(ctx: &Context, workspace: String, tab: String) -> Result<i32> {
    let tab = Tab {
        id: tab,
        close_when_done: true,
    };
    let executable = super::installed::Executable::current();
    // A missing daemon or a failed query does not prove completion or removal.
    // Retain the observer across daemon restarts, but not deletion of its state.
    // A closed tab leaves nothing to close, whatever the daemon reports; a
    // failed tab query, like a failed daemon query, proves neither.
    while ctx.paths.state.exists() {
        let lifecycle = workspace_lifecycle(ctx, &workspace).await;
        let status = match tab.agent_status().await {
            Ok(Some(status)) => Some(status),
            Ok(None) => break,
            Err(_) => None,
        };
        match lifecycle {
            Ok(lifecycle) if status.is_some_and(|status| lifecycle.closes_tab(status)) => {
                tab.close().await;
                break;
            }
            Ok(_) => {}
            Err(error) if error.is::<client::ProtocolMismatch>() => {
                if let Some(failure) = executable.take_over(super::installed::arguments) {
                    return Err(failure);
                }
            }
            Err(_) => {}
        }
        tokio::time::sleep(Duration::from_millis(500)).await;
    }
    Ok(0)
}

#[derive(Debug, PartialEq)]
enum Lifecycle {
    Active,
    /// `stopped` while an agent or command waits for `shoal resume`.
    Completed {
        running: bool,
        stopped: bool,
    },
    Removed,
}

impl Lifecycle {
    /// Completion waits for the agent's final response: its tracked execution
    /// resolves, or Herdr reports that the agent finished its turn. A stopped
    /// agent keeps the tab, which shows why it stopped and where it resumes.
    fn closes_tab(&self, agent: AgentStatus) -> bool {
        match self {
            Self::Active | Self::Completed { stopped: true, .. } => false,
            Self::Completed { running, .. } => {
                !running || matches!(agent, AgentStatus::Idle | AgentStatus::Done)
            }
            Self::Removed => true,
        }
    }
}

async fn workspace_lifecycle(ctx: &Context, workspace: &str) -> Result<Lifecycle> {
    match client::inspect(&ctx.paths, workspace.into()).await {
        Ok(inspection) if inspection.completion.is_some() => Ok(Lifecycle::Completed {
            running: !inspection.executions.is_empty(),
            stopped: crate::execution::recovery::pending(&ctx.paths, workspace)?,
        }),
        Ok(_) => Ok(Lifecycle::Active),
        Err(error) => {
            if client::workspaces(&ctx.paths)
                .await?
                .iter()
                .any(|item| item.id == workspace)
            {
                return Err(error);
            }
            Ok(Lifecycle::Removed)
        }
    }
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        model::{Completion, Execution, Inspection, Workspace},
        paths::Paths,
        protocol::{self, Body, Method, Request, Response},
        state::{ExecutionState, WorkspaceState},
    };
    use tokio::{io::BufReader, net::UnixListener};

    #[tokio::test]
    async fn completion_waits_for_running_and_unresolved_executions() {
        for state in [ExecutionState::Running, ExecutionState::Unknown] {
            let execution = Execution {
                id: "agent".into(),
                workspace_id: "workspace".into(),
                state,
                wrapper: None,
                child: None,
                group_id: None,
            };
            assert_eq!(
                inspect_lifecycle(true, vec![execution]).await,
                Lifecycle::Completed {
                    running: true,
                    stopped: false
                }
            );
        }
        assert_eq!(
            inspect_lifecycle(true, vec![]).await,
            Lifecycle::Completed {
                running: false,
                stopped: false
            }
        );
        assert_eq!(inspect_lifecycle(false, vec![]).await, Lifecycle::Active);
        assert_eq!(
            inspect_with_records(true, vec![], true).await,
            Lifecycle::Completed {
                running: false,
                stopped: true
            }
        );
    }

    #[test]
    fn completion_closes_the_tab_once_the_agent_finishes_its_turn() {
        use AgentStatus::*;
        let completed = |running, stopped| Lifecycle::Completed { running, stopped };
        let running = completed(true, false);
        for status in [Idle, Done] {
            assert!(running.closes_tab(status));
        }
        for status in [Working, Blocked, Unknown] {
            assert!(!running.closes_tab(status));
        }
        for status in [Idle, Working, Blocked, Done, Unknown] {
            assert!(completed(false, false).closes_tab(status));
            assert!(!completed(false, true).closes_tab(status));
            assert!(!completed(true, true).closes_tab(status));
            assert!(Lifecycle::Removed.closes_tab(status));
            assert!(!Lifecycle::Active.closes_tab(status));
        }
    }

    #[test]
    fn only_a_shell_in_the_foreground_is_at_its_prompt() {
        let processes = |foreground: i64| {
            let output = format!(
                r#"{{"id":"cli:pane:process_info","result":{{"process_info":{{"foreground_process_group_id":{foreground},"foreground_processes":[],"pane_id":"w2:p6","shell_pid":58481}},"type":"pane_process_info"}}}}"#
            );
            serde_json::from_str::<ProcessInfo>(&output)
                .unwrap()
                .result
                .process_info
        };
        assert!(processes(58481).shell_in_foreground());
        assert!(!processes(58578).shell_in_foreground());
        let unknown: ProcessInfo =
            serde_json::from_str(r#"{"result":{"process_info":{"pane_id":"w2:p6"}}}"#).unwrap();
        assert!(!unknown.result.process_info.shell_in_foreground());
    }

    #[test]
    fn tabs_without_an_issue_use_the_template_or_branch() {
        let repo = Repository {
            id: "repo".into(),
            path: "/repo".into(),
            source: "https://example.com/owner/shoal.git".into(),
            last_used: 0,
            name: Some("shoal".into()),
            workspaces_dir: None,
        };
        assert_eq!(tab_label(None, &repo, None, "topic"), "topic");
        assert_eq!(
            tab_label(Some("{repo}: {branch}".into()), &repo, None, "topic"),
            "shoal: topic"
        );
    }

    #[test]
    fn tab_info_reads_the_agent_status() {
        let status = |value: &str| {
            let output = format!(
                r#"{{"id":"cli:tab:get","result":{{"tab":{{"agent_status":"{value}","tab_id":"w2:t1"}},"type":"tab_info"}}}}"#
            );
            serde_json::from_str::<TabInfo>(&output)
                .unwrap()
                .result
                .tab
                .agent_status
        };
        assert_eq!(status("done"), AgentStatus::Done);
        assert_eq!(status("idle"), AgentStatus::Idle);
        assert_eq!(status("working"), AgentStatus::Working);
        assert_eq!(status("sleeping"), AgentStatus::Unknown);
        let older: TabInfo =
            serde_json::from_str(r#"{"result":{"tab":{"tab_id":"w2:t1"}}}"#).unwrap();
        assert_eq!(older.result.tab.agent_status, AgentStatus::Unknown);
    }

    #[test]
    fn only_herdr_reporting_the_tab_missing_proves_it_gone() {
        assert!(tab_not_found(
            br#"{"error":{"code":"tab_not_found","message":"tab w2:t9 not found"},"id":"cli:tab:get"}"#
        ));
        assert!(!tab_not_found(
            br#"{"id":"cli:tab:get","error":{"code":"server_not_running","message":"no herdr server is running"}}"#
        ));
        assert!(!tab_not_found(b"connection reset"));
    }

    async fn inspect_lifecycle(completed: bool, executions: Vec<Execution>) -> Lifecycle {
        inspect_with_records(completed, executions, false).await
    }

    async fn inspect_with_records(
        completed: bool,
        executions: Vec<Execution>,
        stopped: bool,
    ) -> Lifecycle {
        let temp = tempfile::tempdir().unwrap();
        let paths = Paths::for_test(temp.path());
        paths.prepare().unwrap();
        if stopped {
            crate::execution::recovery::save_record(&paths, "workspace", "agent", "codex", None)
                .unwrap();
        }
        let listener = UnixListener::bind(&paths.socket).unwrap();
        let server = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let mut stream = BufReader::new(stream);
            let request: Request = protocol::read_buffered(&mut stream).await.unwrap();
            assert!(
                matches!(request.method, Method::InspectWorkspace { workspace } if workspace == "workspace")
            );
            let inspection = Inspection {
                workspace: Workspace::new_record(
                    "repo".into(),
                    "workspace".into(),
                    "/workspace".into(),
                    "topic".into(),
                    WorkspaceState::Ready,
                ),
                completion: completed.then(|| Completion {
                    head: "head".into(),
                    cleanup: false,
                    error: None,
                    keep_request: None,
                }),
                executions,
                issue: None,
                pr_cleanup: None,
                ports: vec![],
                resources: vec![],
                simulators: vec![],
            };
            protocol::write(
                stream.get_mut(),
                &Response::new(request.id, Body::Inspection(inspection)),
            )
            .await
            .unwrap();
        });
        let lifecycle = workspace_lifecycle(&Context::new(paths, true), "workspace")
            .await
            .unwrap();
        server.await.unwrap();
        lifecycle
    }

    #[test]
    fn shell_command_quotes_only_words_that_need_it() {
        let argv: Vec<OsString> = ["/opt/bin/shoal", "internal", "herdr", "a b", "it's", ""]
            .map(Into::into)
            .into();
        assert_eq!(
            shell_command(&argv).unwrap(),
            r#"/opt/bin/shoal internal herdr 'a b' 'it'\''s' ''"#
        );
    }
}
