//! CLI workspace workflows; all state mutations go through the daemon.
use std::{ffi::OsString, path::PathBuf};

use anyhow::{Context as _, Result, bail, ensure};
use serde_json::json;

use crate::{
    cli::{Agent, CodexMode},
    client::{self, request},
    config::Config,
    context::Context,
    env, execution, git,
    happy::{self, HappyAgent},
    hooks::{self, Hook},
    model::Workspace,
    protocol::{Body, Method},
    recovery::ReconcileOptions,
    removal::{BranchChoice, RemovalCheck, RemovalResult},
    shell,
    state::WorkspaceState,
    ui::{self, Fallback},
};

pub(super) async fn pull(ctx: &Context, workspace: Option<String>) -> Result<i32> {
    let workspace = ui::select_workspace(ctx, workspace, Fallback::CurrentDirectory).await?;
    let result = request!(
        &ctx.paths,
        Method::PullDefaultBranch { workspace },
        PulledBranch
    );
    ctx.show(&result, |result| {
        if let Some(skipped) = &result.skipped {
            println!("{skipped}");
        } else if result.updated {
            println!("Updated {} to {}", result.branch, result.commit);
        } else {
            println!(
                "{} is already up to date ({})",
                result.branch, result.commit
            );
        }
    })?;
    Ok(0)
}

pub(super) async fn land(ctx: &Context, workspace: Option<String>) -> Result<i32> {
    let workspace = ui::select_workspace(ctx, workspace, Fallback::CurrentDirectory).await?;
    execution::land(&ctx.paths, workspace, ctx.json).await
}

pub(super) async fn land_worker(ctx: &Context, plan: String) -> Result<i32> {
    anyhow::ensure!(env::is_scoped(), "land worker requires a tracked execution");
    client::call(&ctx.paths, Method::CheckLanding).await?;
    let plan: crate::model::LandPlan = serde_json::from_str(&plan)?;
    anyhow::ensure!(
        std::env::var(env::WORKSPACE_ID)? == plan.workspace.id,
        "land worker requires its authorized workspace"
    );
    let result = crate::repo_git::finish_land(plan).await?;
    ctx.show(&result, |result| {
        let refresh = &result.default_refresh;
        if refresh.updated {
            println!(
                "Updated {} from its upstream ({}..{})",
                refresh.branch, refresh.previous_commit, refresh.commit
            );
        }
        let (branch, default, commit) = (&result.branch, &result.default_branch, &result.commit);
        if !result.updated {
            println!("{default} already contains {branch} ({commit})");
        } else if result.fast_forward {
            println!("Fast-forwarded {default} to {branch} ({commit})");
        } else {
            println!("Merged {branch} into {default} ({commit})");
        }
    })?;
    Ok(0)
}

pub(super) async fn diff(ctx: &Context, workspace: Option<String>) -> Result<i32> {
    let workspace = ui::select_workspace(ctx, workspace, Fallback::CurrentDirectory).await?;
    let base = request!(&ctx.paths, Method::DiffBase { workspace }, DiffBase);
    execution::run(
        &ctx.paths,
        base.workspace_id,
        vec!["git".into(), "diff".into(), base.commit.into(), "--".into()],
    )
    .await
}

pub(super) async fn cd(ctx: &Context, workspace: Option<String>) -> Result<i32> {
    if workspace.as_deref() == Some("-") {
        let destination = shell::previous_directory()?;
        if env::is_scoped() {
            let workspaces = client::workspaces(&ctx.paths).await?;
            ensure!(
                workspaces.iter().any(|w| w.contains(&destination)),
                "workspace processes cannot navigate outside their worktree"
            );
        }
        navigate(ctx, &destination)?;
    } else {
        let workspace = match workspace {
            Some(workspace) => workspace,
            None => ui::workspace_picker(ctx).await?,
        };
        let inspection = client::inspect(&ctx.paths, workspace).await?;
        ensure!(
            inspection.workspace.path.is_dir(),
            "workspace directory is missing"
        );
        navigate(ctx, &inspection.workspace.path)?;
    }
    Ok(0)
}

/// Report the destination and ask the shell wrapper to change directory.
fn navigate(ctx: &Context, path: &std::path::Path) -> Result<()> {
    ctx.emit(&path.display().to_string(), json!({"path": path}))?;
    shell::navigate(path, ctx.json)
}

pub(super) async fn add(
    ctx: &Context,
    repository: Option<String>,
    names: (Option<String>, Option<String>),
    base: Option<String>,
    issue: Option<String>,
    agent: Option<Agent>,
    mut args: Vec<OsString>,
) -> Result<i32> {
    let (name, mut branch) = names;
    // Validate launch configuration before creating a workspace.
    let codex_mode = match agent {
        Some(Agent::Codex) if issue.is_some() => Some(CodexMode::Cli),
        Some(Agent::Codex) => Some(Config::load(&ctx.paths)?.codex.default_mode),
        _ => None,
    };
    let repository = match repository {
        Some(repo) => ui::repository_selector(repo)?,
        None => ui::pick(
            ctx,
            "Repository> ",
            ui::repository_choices(client::repositories(&ctx.paths).await?).await?,
        )?,
    };
    let issue = match issue {
        Some(input) => {
            let repos = client::repositories(&ctx.paths).await?;
            let repo = crate::repository::select(&repos, &repository).await?;
            Some(super::issues::load(repo, &input).await?)
        }
        None => None,
    };
    let mut name = name.or_else(|| issue.as_ref().map(|issue| issue.branch_name()));
    if name.is_none() && branch.is_none() && base.is_none() && ctx.interactive() {
        let mode = ui::pick(
            ctx,
            "Workspace> ",
            vec![
                ("new".into(), "Create a new branch".into()),
                ("existing".into(), "Use an existing branch".into()),
            ],
        )?;
        if mode == "existing" {
            let branches = request!(
                &ctx.paths,
                Method::ListBranches {
                    repository: repository.clone()
                },
                Branches
            );
            let workspaces = client::workspaces(&ctx.paths).await?;
            let repos = client::repositories(&ctx.paths).await?;
            let repo = crate::repository::select(&repos, &repository).await?;
            let entries = branches
                .into_iter()
                .map(|b| {
                    let label = match &b.remote {
                        Some(remote) => format!("{remote}/{}  (remote)", b.name),
                        None => format!("{}  (local)", b.name),
                    };
                    let label = match workspaces
                        .iter()
                        .find(|w| w.repository_id == repo.id && w.branch == b.name)
                    {
                        Some(w) => format!("{label}  [workspace: {}]", w.name),
                        None => label,
                    };
                    (b.selector(), label)
                })
                .collect();
            branch = Some(ui::pick(ctx, "Branch> ", entries)?);
        }
    }
    // Terminal agents take the issue prompt as their first argument; Happy
    // sessions decide per agent whether one can be delivered.
    let prompt = issue
        .filter(|_| agent.is_some())
        .map(|issue| issue.prompt());
    if let Some(prompt) = &prompt
        && !matches!(agent, Some(Agent::Happy(_)))
    {
        args.insert(0, prompt.into());
    }
    let (mut workspace, reused) = if let Some(branch) = branch {
        let opened = request!(
            &ctx.paths,
            Method::OpenBranch { repository, branch },
            OpenedWorkspace
        );
        (opened.workspace, opened.reused)
    } else {
        let name = match name.take() {
            Some(name) => name,
            // The daemon rejects bad syntax too; checking here lets a typo be
            // corrected instead of ending the command.
            None => loop {
                let name = ui::input(ctx, "Branch name")?;
                match git::check_branch_name(None, &name).await {
                    Ok(()) => break name,
                    Err(error) => eprintln!("{error:#}"),
                }
            },
        };
        (
            request!(
                &ctx.paths,
                Method::CreateWorkspace {
                    repository,
                    name,
                    base
                },
                Workspace
            ),
            false,
        )
    };
    if workspace.state == WorkspaceState::Preparing {
        let Some(prepared) = prepare_workspace(ctx, &workspace).await? else {
            return Ok(1);
        };
        workspace = prepared;
    }
    ctx.emit(
        &format!(
            "{} {} on branch {} at {}",
            if reused { "Opened" } else { "Created" },
            workspace.name,
            workspace.branch,
            workspace.path.display()
        ),
        &workspace,
    )?;
    shell::navigate(&workspace.path, ctx.json)?;
    if !reused {
        run_post_setup(ctx, &workspace).await?;
    }
    match agent {
        Some(Agent::Codex) => codex(ctx, codex_mode, Some(workspace.id), args).await,
        Some(Agent::Claude) => claude(ctx, Some(workspace.id), args).await,
        Some(Agent::Happy(agent)) => happy(ctx, agent, Some(workspace.id), prompt, args).await,
        None => Ok(0),
    }
}

pub(super) async fn prepare(ctx: &Context, workspace: Option<String>) -> Result<i32> {
    let workspace = ui::select_workspace(ctx, workspace, Fallback::CurrentDirectory).await?;
    let inspection = client::inspect(&ctx.paths, workspace).await?;
    let Some(workspace) = prepare_workspace(ctx, &inspection.workspace).await? else {
        return Ok(1);
    };
    run_post_setup(ctx, &workspace).await?;
    ctx.emit(&format!("Prepared {}", workspace.name), &workspace)?;
    Ok(0)
}

/// Run the repository's `post_setup_cmd`, if any, once the workspace is ready.
/// A failure keeps the ready workspace and stops what would follow.
async fn run_post_setup(ctx: &Context, workspace: &Workspace) -> Result<()> {
    let hooks = request!(
        &ctx.paths,
        Method::WorkspaceHooks {
            workspace: workspace.id.clone(),
        },
        Hooks
    );
    if let Some(command) = hooks.post_setup_cmd {
        hooks::run_interactive(Hook::PostSetup, workspace, &command, &ctx.paths, ctx.json).await?;
    }
    Ok(())
}

/// Run the setup command, then let an interactive user decide what to do with
/// a failed workspace. `None` means the workspace was deleted or kept as-is.
async fn prepare_workspace(ctx: &Context, workspace: &Workspace) -> Result<Option<Workspace>> {
    let failure = match execution::prepare(&ctx.paths, workspace.id.clone(), ctx.json).await {
        Ok(0) => None,
        Ok(code) => Some(format!("setup command exited with status {code}")),
        Err(error) => Some(format!("{error:#}")),
    };
    if let Some(error) = failure {
        let name = &workspace.name;
        ensure!(
            ctx.interactive(),
            "setup failed for {name}: {error}; workspace retained. Retry with `shoal prepare {name}`, ignore with `shoal reconcile {name} --repair`, or delete with `shoal rm {name} --yes --delete-branch`"
        );
        eprintln!("Setup failed for {name}: {error}");
        match ui::setup_failure_choice()? {
            ui::SetupFailureChoice::Delete => {
                delete_failed_workspace(ctx, workspace).await?;
                return Ok(None);
            }
            ui::SetupFailureChoice::Ignore => ignore_setup_failure(ctx, workspace).await?,
            ui::SetupFailureChoice::Cancel => return Ok(None),
        }
    }
    let inspection = client::inspect(&ctx.paths, workspace.id.clone()).await?;
    ensure!(
        inspection.workspace.state == WorkspaceState::Ready,
        "workspace setup did not complete"
    );
    Ok(Some(inspection.workspace))
}

async fn delete_failed_workspace(ctx: &Context, workspace: &Workspace) -> Result<()> {
    let confirmed = ui::confirm(
        ctx,
        &format!(
            "Delete workspace {} at {} and its branch {} (including setup changes)?",
            workspace.name,
            workspace.path.display(),
            workspace.branch
        ),
        "--yes",
    )?;
    if confirmed {
        client::call(
            &ctx.paths,
            Method::RemoveWorkspace {
                workspace: workspace.id.clone(),
                choice: BranchChoice::DeleteBranch,
                caller_pid: std::process::id(),
            },
        )
        .await?;
        eprintln!("Deleted workspace {}", workspace.name);
    }
    Ok(())
}

async fn ignore_setup_failure(ctx: &Context, workspace: &Workspace) -> Result<()> {
    let reports = request!(
        &ctx.paths,
        Method::Reconcile {
            workspace: Some(workspace.id.clone()),
            options: ReconcileOptions {
                repair: true,
                ..Default::default()
            },
        },
        Reconciliation
    );
    ensure!(
        reports
            .iter()
            .all(|r| r.workspace.state == WorkspaceState::Ready),
        "workspace still has unresolved ownership or processes; inspect with shoal reconcile"
    );
    Ok(())
}

pub(super) async fn list(ctx: &Context) -> Result<i32> {
    let workspaces = client::workspaces(&ctx.paths).await?;
    ctx.show(&workspaces, |workspaces| {
        for workspace in workspaces {
            println!("{}", ui::workspace_label(workspace));
        }
    })?;
    Ok(0)
}

pub(super) async fn inspect(ctx: &Context, workspace: Option<String>) -> Result<i32> {
    let workspace = ui::select_workspace(ctx, workspace, Fallback::Picker).await?;
    let inspection = client::inspect(&ctx.paths, workspace).await?;
    ctx.show(&inspection, |inspection| {
        println!(
            "{}",
            serde_json::to_string_pretty(inspection).unwrap_or_default()
        );
    })?;
    Ok(0)
}

pub(super) async fn stop(ctx: &Context, workspace: Option<String>) -> Result<i32> {
    let workspace = ui::select_workspace(ctx, workspace, Fallback::Picker).await?;
    client::call(&ctx.paths, Method::StopWorkspace { workspace }).await?;
    ctx.emit("Workspace processes stopped", json!({"stopped": true}))?;
    Ok(0)
}

pub(super) async fn remove(
    ctx: &Context,
    workspace: Option<String>,
    yes: bool,
    keep_branch: bool,
    delete_branch: bool,
) -> Result<i32> {
    let workspace = ui::select_workspace(ctx, workspace, Fallback::CurrentDirectory).await?;
    let caller_pid = std::process::id();
    let check = request!(
        &ctx.paths,
        Method::CheckRemoval {
            workspace: workspace.clone(),
            caller_pid,
        },
        RemovalCheck
    );
    let choice = if keep_branch {
        BranchChoice::KeepBranch
    } else if delete_branch {
        BranchChoice::DeleteBranch
    } else if !check.needs_choice() {
        BranchChoice::Auto
    } else {
        ensure!(
            !yes,
            "choose --keep-branch or --delete-branch with --yes for a dirty or differing workspace"
        );
        ui::choose_removal(ctx, &check)?
    };
    if !yes && (check.needs_choice() || keep_branch || delete_branch) {
        confirm_removal(ctx, &check, choice)?;
    }
    // Leave the directory before it disappears under the shell.
    let escape = escape_destination(ctx, &check.workspace).await?;
    let result = client::call(
        &ctx.paths,
        Method::RemoveWorkspace {
            workspace,
            choice,
            caller_pid,
        },
    )
    .await;
    if let Some(destination) = escape {
        if result.is_ok() || !std::env::current_dir().is_ok_and(|cwd| cwd.exists()) {
            shell::navigate(&destination, ctx.json)?;
        }
    }
    let Body::RemovalResult(result) = result? else {
        bail!("unexpected removal response");
    };
    ctx.emit(&removal_message(&result), &result)?;
    Ok(0)
}

fn confirm_removal(ctx: &Context, check: &RemovalCheck, choice: BranchChoice) -> Result<()> {
    let branch_action = match choice {
        BranchChoice::KeepBranch => "keep",
        BranchChoice::DeleteBranch => "delete (including unpushed commits)",
        BranchChoice::Auto => "delete (redundant)",
    };
    ensure!(
        ui::confirm(
            ctx,
            &format!(
                "Remove workspace: {}\nFiles:  delete, including uncommitted changes\nBranch: {} — {branch_action}",
                check.workspace.name,
                check.branch.as_deref().unwrap_or("none")
            ),
            "--yes",
        )?,
        "workspace removal canceled"
    );
    Ok(())
}

/// Where the shell should go if the current directory is inside `workspace`:
/// its repository's Shoal directory, or home when that is unavailable.
async fn escape_destination(ctx: &Context, workspace: &Workspace) -> Result<Option<PathBuf>> {
    let cwd = std::env::current_dir()?;
    if !workspace.contains(&cwd) {
        return Ok(None);
    }
    let repository = client::repositories(&ctx.paths)
        .await?
        .into_iter()
        .find(|r| r.id == workspace.repository_id)
        .and_then(|r| r.workspaces_dir)
        .filter(|p| p.is_dir());
    Ok(Some(repository.unwrap_or_else(|| ctx.paths.home.clone())))
}

fn removal_message(result: &RemovalResult) -> String {
    match (&result.branch, result.branch_deleted) {
        (Some(branch), true) => format!("Workspace and Git branch {branch} removed"),
        (Some(branch), false) => format!(
            "Workspace removed; Git branch {branch} retained ({})",
            result.branch_outcome
        ),
        (None, _) => "Workspace removed".into(),
    }
}

pub(super) async fn exec(
    ctx: &Context,
    workspace: Option<String>,
    command: Vec<OsString>,
) -> Result<i32> {
    let workspace = ui::select_workspace(ctx, workspace, Fallback::CurrentDirectory).await?;
    execution::run(&ctx.paths, workspace, command).await
}

pub(super) async fn claude(
    ctx: &Context,
    workspace: Option<String>,
    args: Vec<OsString>,
) -> Result<i32> {
    let workspace = ui::select_workspace(ctx, workspace, Fallback::CurrentDirectory).await?;
    let inspection = client::inspect(&ctx.paths, workspace).await?;
    trust_claude(ctx, &inspection.workspace.path);
    let command = std::iter::once("claude".into())
        .chain(args)
        .chain(["--remote-control".into(), inspection.workspace.name.into()])
        .collect();
    execution::run(&ctx.paths, inspection.workspace.id, command).await
}

/// Mark the workspace trusted in Claude Code's config so a launch, attached or
/// detached, never stops at the trust dialog. Failure only warns.
fn trust_claude(ctx: &Context, workspace: &std::path::Path) {
    let trusted = env::claude_config_dir().and_then(|dir| {
        let config = dir
            .unwrap_or_else(|| ctx.paths.home.clone())
            .join(".claude.json");
        trust_claude_workspace(&config, workspace)
    });
    if let Err(error) = trusted {
        eprintln!("warning: could not mark the workspace as trusted for Claude Code: {error:#}");
    }
}

pub(super) async fn codex(
    ctx: &Context,
    mode: Option<CodexMode>,
    workspace: Option<String>,
    args: Vec<OsString>,
) -> Result<i32> {
    let mode = match mode {
        Some(mode) => mode,
        None => Config::load(&ctx.paths)?.codex.default_mode,
    };
    if mode == CodexMode::App {
        return open_app(ctx, workspace, "codex", args).await;
    }
    let workspace = ui::select_workspace(ctx, workspace, Fallback::CurrentDirectory).await?;
    let command = std::iter::once("codex".into())
        .chain(args)
        .chain([
            "--sandbox".into(),
            "danger-full-access".into(),
            "--ask-for-approval=never".into(),
        ])
        .collect();
    execution::run(&ctx.paths, workspace, command).await
}

/// Start a Happy session the way Happy's own daemon does, so it registers with
/// that daemon and appears in the app, but detached from this terminal and
/// tracked like any other workspace command. The CLI returns once the launch
/// is recorded; the session's output goes to a log under Shoal's state.
pub(super) async fn happy(
    ctx: &Context,
    agent: HappyAgent,
    workspace: Option<String>,
    prompt: Option<String>,
    args: Vec<OsString>,
) -> Result<i32> {
    let workspace = ui::select_workspace(ctx, workspace, Fallback::CurrentDirectory).await?;
    let workspace = client::inspect(&ctx.paths, workspace).await?.workspace;
    if agent == HappyAgent::Claude {
        trust_claude(ctx, &workspace.path);
    }
    let daemon_state = happy::daemon_state_path(&ctx.paths.home);
    let daemon_recorded = daemon_state.is_file();
    if !daemon_recorded {
        eprintln!(
            "warning: Happy daemon state not found at {}; the session will not appear in the Happy app until `happy daemon start` runs",
            daemon_state.display()
        );
    }
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|elapsed| elapsed.as_millis())
        .unwrap_or_default();
    let state_dir = ctx.paths.workspace_state(&workspace.id);
    let stem = format!("happy-{}-{stamp}", agent.name());
    let log = state_dir.join(format!("{stem}.log"));
    // Happy's Codex mode has no prompt argument: the prompt is kept in a file
    // and, when this machine is logged in to Happy, delivered through Happy's
    // server to a session Shoal creates for the agent to attach to.
    let mut prompt_file = None;
    let mut seeded = None;
    let mut env = Vec::new();
    if let Some(prompt) = &prompt
        && !agent.accepts_prompt()
    {
        let path = state_dir.join(format!("{stem}.prompt.md"));
        std::fs::create_dir_all(&state_dir)
            .with_context(|| format!("create {}", state_dir.display()))?;
        std::fs::write(&path, prompt).with_context(|| format!("write {}", path.display()))?;
        match happy::client::seed(&ctx.paths, &workspace, agent).await {
            Ok(session) => {
                env = session.session.env();
                seeded = Some(session);
            }
            Err(error) => eprintln!(
                "warning: cannot deliver the prompt through Happy ({error:#}); send the prompt saved at {} to the session yourself",
                path.display()
            ),
        }
        prompt_file = Some(path);
    }
    let command = happy::command(agent, prompt.as_deref(), args);
    let launch = execution::launch_detached(&ctx.paths, &workspace, log, command, &env).await?;
    let mut prompt_delivered = false;
    if let (Some(seeded), Some(prompt)) = (&seeded, &prompt) {
        match seeded
            .deliver(prompt, std::time::Duration::from_secs(90))
            .await
        {
            Ok(()) => prompt_delivered = true,
            Err(error) => eprintln!(
                "warning: prompt not delivered ({error:#}); send the prompt saved at {} to the session yourself",
                prompt_file
                    .as_deref()
                    .map(std::path::Path::display)
                    .unwrap()
            ),
        }
    }
    let delivery = match (&prompt_file, prompt_delivered) {
        (Some(_), true) => "\nPrompt delivered to the session",
        (Some(_), false) => "\nPrompt saved for you to send from the app",
        (None, _) => "",
    };
    ctx.emit(
        &format!(
            "Started Happy {} session in {} (execution {}, pid {})\nOutput: {}{delivery}",
            agent.name(),
            workspace.name,
            launch.execution_id,
            launch.pid,
            launch.log.display()
        ),
        json!({
            "workspace": workspace,
            "agent": agent,
            "execution_id": launch.execution_id,
            "pid": launch.pid,
            "log": launch.log,
            "prompt_file": prompt_file,
            "prompt_delivered": prompt_delivered,
            "happy_session_id": seeded.as_ref().map(|seeded| seeded.session.id.clone()),
            "happy_daemon_recorded": daemon_recorded,
        }),
    )?;
    Ok(0)
}

/// Desktop launchers hand the directory to another process. Their short-lived
/// command is not the agent session and must not own/kill the app's process group.
pub(super) async fn open_app(
    ctx: &Context,
    workspace: Option<String>,
    program: &str,
    args: Vec<OsString>,
) -> Result<i32> {
    let workspace = ui::select_workspace(ctx, workspace, Fallback::CurrentDirectory).await?;
    let inspection = client::inspect(&ctx.paths, workspace).await?;
    ensure!(
        inspection.workspace.path.is_dir(),
        "workspace directory is missing"
    );
    let status = tokio::process::Command::new(program)
        .arg("app")
        .arg(&inspection.workspace.path)
        .args(args)
        .current_dir(&inspection.workspace.path)
        .env_remove(env::SHELL_DIRECTIVE)
        .status()
        .await
        .with_context(|| {
            format!("launch {program} app; install {program} and make it available on PATH")
        })?;
    Ok(execution::exit_code(status))
}

/// Record the workspace as trusted in Claude Code's `.claude.json` so
/// `claude` starts without its workspace trust dialog. Returns whether the
/// file changed; a missing file is left for Claude Code to create.
fn trust_claude_workspace(config: &std::path::Path, workspace: &std::path::Path) -> Result<bool> {
    use serde_json::Value;
    let workspace = std::fs::canonicalize(workspace).unwrap_or_else(|_| workspace.to_owned());
    let key = workspace
        .to_str()
        .context("workspace path is not UTF-8")?
        .to_owned();
    let text = match std::fs::read_to_string(config) {
        Ok(text) => text,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(false),
        Err(error) => return Err(error).with_context(|| format!("read {}", config.display())),
    };
    let mut root: Value =
        serde_json::from_str(&text).with_context(|| format!("parse {}", config.display()))?;
    let project = root
        .as_object_mut()
        .context("Claude config is not a JSON object")?
        .entry("projects")
        .or_insert_with(|| json!({}))
        .as_object_mut()
        .context("Claude config `projects` is not a JSON object")?
        .entry(key)
        .or_insert_with(|| json!({}))
        .as_object_mut()
        .context("Claude project entry is not a JSON object")?;
    if project.get("hasTrustDialogAccepted") == Some(&Value::Bool(true)) {
        return Ok(false);
    }
    project.insert("hasTrustDialogAccepted".into(), Value::Bool(true));
    let directory = config.parent().context("Claude config has no parent")?;
    let mut temporary = tempfile::NamedTempFile::new_in(directory)?;
    serde_json::to_writer_pretty(&mut temporary, &root)?;
    temporary
        .as_file()
        .set_permissions(std::fs::metadata(config)?.permissions())?;
    temporary
        .persist(config)
        .with_context(|| format!("replace {}", config.display()))?;
    Ok(true)
}

pub(super) async fn pr(
    ctx: &Context,
    workspace: Option<String>,
    url: Option<String>,
    clear: bool,
) -> Result<i32> {
    let workspace = ui::select_workspace(ctx, workspace, Fallback::CurrentDirectory).await?;
    let body = client::call(
        &ctx.paths,
        Method::SetPr {
            workspace,
            url,
            clear,
        },
    )
    .await?;
    ensure!(
        matches!(body, Body::Ok),
        "unexpected PR cleanup response: {body:?}"
    );
    ctx.emit(
        if clear {
            "PR cleanup cancelled"
        } else {
            "PR cleanup registered; tracked commands will stop when the merge is confirmed"
        },
        serde_json::json!({"registered": !clear}),
    )?;
    Ok(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::Value;
    use std::fs;

    #[test]
    fn claude_trust_adds_the_project_once_and_preserves_other_settings() {
        let dir = tempfile::tempdir().unwrap();
        let config = dir.path().join(".claude.json");
        let workspace = dir.path().join("ws");
        fs::create_dir(&workspace).unwrap();
        assert!(!trust_claude_workspace(&config, &workspace).unwrap());
        assert!(!config.exists());
        fs::write(
            &config,
            r#"{"numStartups": 3, "projects": {"/other": {"allowedTools": ["Bash"], "hasTrustDialogAccepted": false}}}"#,
        )
        .unwrap();
        assert!(trust_claude_workspace(&config, &workspace).unwrap());
        assert!(!trust_claude_workspace(&config, &workspace).unwrap());
        let root: Value = serde_json::from_str(&fs::read_to_string(&config).unwrap()).unwrap();
        assert_eq!(root["numStartups"], 3);
        assert_eq!(root["projects"]["/other"]["allowedTools"], json!(["Bash"]));
        assert_eq!(root["projects"]["/other"]["hasTrustDialogAccepted"], false);
        let key = fs::canonicalize(&workspace).unwrap();
        assert_eq!(
            root["projects"][key.to_str().unwrap()]["hasTrustDialogAccepted"],
            true
        );
        fs::write(&config, "{not json").unwrap();
        assert!(trust_claude_workspace(&config, &workspace).is_err());
        fs::write(&config, r#"{"projects": []}"#).unwrap();
        assert!(trust_claude_workspace(&config, &workspace).is_err());
    }
}
