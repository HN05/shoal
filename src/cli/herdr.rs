//! Herdr terminal handoffs; workspace ownership stays with the daemon.
use std::{ffi::OsString, path::Path, process::Stdio, time::Duration};

use anyhow::{Context as _, Result, ensure};
use serde::{Deserialize, Serialize};
use tokio::process::Command;

use super::{
    client,
    commands::{
        issues::Issue,
        workspaces::{AddPlan, execute_add},
    },
    context::Context,
    internal,
};
use crate::{config::Config, env, paths::Paths, protocol::ConfigTarget, subprocess::Run};

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

pub(super) async fn handoff(
    ctx: &Context,
    plan: &mut AddPlan,
    issue: Option<&Issue>,
    here: bool,
) -> Result<bool> {
    if here || !available(ctx) {
        return Ok(false);
    }
    let settings = client::settings(
        &ctx.paths,
        ConfigTarget::Repository(plan.repository.clone()),
    )
    .await?;
    if !settings.herdr.new_tab {
        return Ok(false);
    }
    ensure!(
        !env::inherits_scope(&ctx.paths.state),
        "workspace processes cannot allocate workspaces"
    );
    let focus = settings.herdr.focus.unwrap_or(!plan.runs_unattended());
    if let Some(template) = settings.herdr.tab_name {
        let repos = client::repositories(&ctx.paths).await?;
        let repo = crate::forge::repository::select(&repos, &plan.repository).await?;
        plan.tab_name = Some(TabName {
            template,
            repo: crate::forge::repository::name(repo).to_owned(),
            issue_number: issue.map(|issue| issue.number),
            issue_title: issue.map(|issue| issue.title.clone()),
        });
    }
    let created = create_tab(ctx, plan, focus).await?;
    submit_worker(ctx, created, settings.herdr.close_when_done).await?;
    Ok(true)
}

/// Whether this terminal can hand Shoal work to new Herdr tabs.
pub(in crate::cli) fn available(ctx: &Context) -> bool {
    ctx.herdr_tab.is_none() && ctx.interactive() && std::env::var("HERDR_ENV").as_deref() == Ok("1")
}

/// Run Shoal with `args` in a new background tab at `cwd`.
pub(in crate::cli) async fn run_in_tab(
    ctx: &Context,
    cwd: &Path,
    label: &str,
    args: &[OsString],
) -> Result<()> {
    let created = created(tab_command(ctx, cwd, label, false)?).await?;
    let mut argv = shoal_argv(&ctx.paths)?;
    argv.extend_from_slice(args);
    run_in_pane(&created, &argv).await
}

// The tab environment carries the plan, keeping the typed command short.
async fn create_tab(ctx: &Context, plan: &AddPlan, focus: bool) -> Result<CreatedResult> {
    let mut create = tab_command(ctx, &std::env::current_dir()?, &plan.label(), focus)?;
    create.arg("--env").arg(format!(
        "{}={}",
        env::HERDR_PLAN,
        serde_json::to_string(plan)?
    ));
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
    let mut argv = shoal_argv(&ctx.paths)?;
    argv.push(internal::HERDR.into());
    if close_when_done {
        argv.push("--close-when-done".into());
    }
    run_in_pane(&created, &argv).await
}

async fn run_in_pane(created: &CreatedResult, argv: &[OsString]) -> Result<()> {
    let mut run = Command::new("herdr");
    run.args(["pane", "run"])
        .arg(&created.root_pane.pane_id)
        .arg(shell_command(argv)?);
    Run::new(run)
        .checked()
        .await
        .context("run Shoal in the new Herdr tab (tab retained)")?;
    Ok(())
}

// The tab inherits HOME, so only a non-default state directory needs naming.
fn shoal_argv(paths: &Paths) -> Result<Vec<OsString>> {
    let mut argv = vec![std::env::current_exe()?.into_os_string()];
    if !paths.is_default_state() {
        argv.extend(["--state-dir".into(), paths.state.clone().into_os_string()]);
    }
    Ok(argv)
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
    ctx.herdr_tab = Some(Tab {
        id,
        close_when_done,
    });
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

    /// The tab's agent state; `None` once the tab is gone.
    async fn agent_status(&self) -> Option<AgentStatus> {
        let mut get = Command::new("herdr");
        get.args(["tab", "get"]).arg(&self.id);
        let output = Run::new(get).checked().await.ok()?;
        Some(
            serde_json::from_slice::<TabInfo>(&output.stdout)
                .map_or(AgentStatus::Unknown, |info| info.result.tab.agent_status),
        )
    }

    pub async fn shell(&self, cwd: &Path) -> Result<i32> {
        let shell = std::env::var_os("SHELL")
            .filter(|shell| !shell.is_empty())
            .unwrap_or_else(|| "/bin/sh".into());
        let status = Command::new(shell)
            .arg("-i")
            .current_dir(cwd)
            .env_remove(env::SHELL_DIRECTIVE)
            .status()
            .await
            .context("open workspace shell")?;
        Ok(crate::execution::exit_code(status))
    }

    /// Observe lifecycle separately so returning from an agent leaves the tab open.
    pub fn watch_workspace(&self, ctx: &Context, workspace: &str) -> Result<()> {
        if !self.close_when_done {
            return Ok(());
        }
        let argv = internal::internal_command(
            &ctx.paths,
            ctx.json,
            internal::InternalCommand::HerdrWatch {
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
    let executable = Executable::current();
    // A missing daemon or a failed query does not prove completion or removal.
    // Retain the observer across daemon restarts, but not deletion of its state.
    while ctx.paths.state.exists() {
        match workspace_lifecycle(ctx, &workspace).await {
            Ok(Lifecycle::Active) => {}
            Ok(lifecycle) => match tab.agent_status().await {
                None => break,
                Some(status) if lifecycle.closes_tab(status) => {
                    tab.close().await;
                    break;
                }
                Some(_) => {}
            },
            Err(error) if error.is::<client::ProtocolMismatch>() => {
                if let Some(installed) = executable.as_ref().filter(|exe| exe.replaced()) {
                    return Err(installed.exec());
                }
            }
            Err(_) => {}
        }
        tokio::time::sleep(Duration::from_millis(500)).await;
    }
    Ok(0)
}

/// The binary this watcher runs, so an upgrade that changes the daemon
/// protocol can hand the watch to the installed release.
struct Executable {
    path: std::path::PathBuf,
    identity: Option<FileIdentity>,
}

type FileIdentity = (u64, u64, i64, i64);

impl Executable {
    fn current() -> Option<Self> {
        Some(Self::at(std::env::current_exe().ok()?))
    }

    fn at(path: std::path::PathBuf) -> Self {
        let identity = file_identity(&path);
        Self { path, identity }
    }

    /// Whether another binary now sits at this path, as after an upgrade.
    fn replaced(&self) -> bool {
        file_identity(&self.path).is_some_and(|now| self.identity != Some(now))
    }

    /// Restart this command from the installed binary; returns only on failure.
    fn exec(&self) -> anyhow::Error {
        use std::os::unix::process::CommandExt as _;
        let error = std::process::Command::new(&self.path)
            .args(std::env::args_os().skip(1))
            .exec();
        anyhow::Error::new(error).context("restart the Herdr tab watcher from the installed shoal")
    }
}

// Follows symlinks, so retargeting a versioned install link counts as a change.
fn file_identity(path: &Path) -> Option<FileIdentity> {
    use std::os::unix::fs::MetadataExt as _;
    let metadata = std::fs::metadata(path).ok()?;
    Some((
        metadata.dev(),
        metadata.ino(),
        metadata.mtime(),
        metadata.mtime_nsec(),
    ))
}

#[derive(Debug, PartialEq)]
enum Lifecycle {
    Active,
    Completed { running: bool },
    Removed,
}

impl Lifecycle {
    /// Completion waits for the agent's final response: its tracked execution
    /// resolves, or Herdr reports that the agent finished its turn.
    fn closes_tab(&self, agent: AgentStatus) -> bool {
        match self {
            Self::Active => false,
            Self::Completed { running } => {
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
                Lifecycle::Completed { running: true }
            );
        }
        assert_eq!(
            inspect_lifecycle(true, vec![]).await,
            Lifecycle::Completed { running: false }
        );
        assert_eq!(inspect_lifecycle(false, vec![]).await, Lifecycle::Active);
    }

    #[test]
    fn completion_closes_the_tab_once_the_agent_finishes_its_turn() {
        use AgentStatus::*;
        let running = Lifecycle::Completed { running: true };
        for status in [Idle, Done] {
            assert!(running.closes_tab(status));
        }
        for status in [Working, Blocked, Unknown] {
            assert!(!running.closes_tab(status));
        }
        for status in [Idle, Working, Blocked, Done, Unknown] {
            assert!(Lifecycle::Completed { running: false }.closes_tab(status));
            assert!(Lifecycle::Removed.closes_tab(status));
            assert!(!Lifecycle::Active.closes_tab(status));
        }
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

    async fn inspect_lifecycle(completed: bool, executions: Vec<Execution>) -> Lifecycle {
        let temp = tempfile::tempdir().unwrap();
        let paths = Paths::for_test(temp.path());
        paths.prepare().unwrap();
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
    fn executable_is_replaced_by_a_new_file_or_link_target() {
        let temp = tempfile::tempdir().unwrap();
        let first = temp.path().join("first");
        let second = temp.path().join("second");
        let link = temp.path().join("shoal");
        std::fs::write(&first, "old").unwrap();
        std::fs::write(&second, "new").unwrap();
        std::os::unix::fs::symlink(&first, &link).unwrap();
        let executable = Executable::at(link.clone());
        assert!(!executable.replaced());

        std::fs::remove_file(&link).unwrap();
        std::os::unix::fs::symlink(&second, &link).unwrap();
        assert!(executable.replaced());

        let executable = Executable::at(first.clone());
        std::fs::rename(&second, &first).unwrap();
        assert!(executable.replaced());
        std::fs::remove_file(&first).unwrap();
        assert!(!executable.replaced(), "a missing binary cannot take over");
    }

    #[test]
    fn shoal_argv_names_only_a_non_default_state_directory() {
        let custom = Paths::for_test("/home");
        let default = Paths {
            state: "/home/.local/state/shoal".into(),
            ..custom.clone()
        };
        assert_eq!(shoal_argv(&default).unwrap().len(), 1);
        assert_eq!(
            shoal_argv(&custom).unwrap()[1..],
            ["--state-dir", "/home/state"]
        );
    }

    #[test]
    fn shell_command_quotes_only_words_that_need_it() {
        let argv: Vec<OsString> = ["/opt/bin/shoal", "herdr-internal", "a b", "it's", ""]
            .map(Into::into)
            .into();
        assert_eq!(
            shell_command(&argv).unwrap(),
            r#"/opt/bin/shoal herdr-internal 'a b' 'it'\''s' ''"#
        );
    }
}
