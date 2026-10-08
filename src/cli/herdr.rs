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
    let focus = settings.herdr.focus.unwrap_or(!plan.launches_agent());
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

    async fn exists(&self) -> bool {
        let mut get = Command::new("herdr");
        get.args(["tab", "get"]).arg(&self.id);
        Run::new(get).checked().await.is_ok()
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
    // A missing daemon or a failed query does not prove completion or removal.
    // Retain the observer across daemon restarts, but not deletion of its state.
    while ctx.paths.state.exists() {
        if workspace_finished(ctx, &workspace).await.unwrap_or(false) {
            if tab.exists().await {
                tab.close().await;
            }
            break;
        }
        tokio::time::sleep(Duration::from_millis(500)).await;
    }
    Ok(0)
}

async fn workspace_finished(ctx: &Context, workspace: &str) -> Result<bool> {
    match client::inspect(&ctx.paths, workspace.into()).await {
        Ok(inspection) => Ok(inspection.completion.is_some() && inspection.executions.is_empty()),
        Err(error) => {
            if client::workspaces(&ctx.paths)
                .await?
                .iter()
                .any(|item| item.id == workspace)
            {
                return Err(error);
            }
            Ok(true)
        }
    }
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
            assert!(!inspect_finished(true, vec![execution]).await);
        }
        assert!(inspect_finished(true, vec![]).await);
        assert!(!inspect_finished(false, vec![]).await);
    }

    async fn inspect_finished(completed: bool, executions: Vec<Execution>) -> bool {
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
                manual_completion: false,
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
        let finished = workspace_finished(&Context::new(paths, true), "workspace")
            .await
            .unwrap();
        server.await.unwrap();
        finished
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
