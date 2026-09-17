//! CLI workspace workflows; all state mutations go through the daemon.
use std::{ffi::OsString, path::PathBuf};

use anyhow::{Context as _, Result, bail, ensure};
use serde_json::json;

use crate::{
    cli::{Agent, CodexMode},
    client::{self, request},
    config::Config,
    context::Context,
    env, execution,
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
        if result.updated {
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
    name: Option<String>,
    base: Option<String>,
    agent: Option<Agent>,
    args: Vec<OsString>,
) -> Result<i32> {
    // Validate launch configuration before creating a workspace.
    let codex_mode = match agent {
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
    let name = match name {
        Some(name) => name,
        None => ui::input(ctx, "Branch name")?,
    };
    let mut workspace = request!(
        &ctx.paths,
        Method::CreateWorkspace {
            repository,
            name,
            base,
        },
        Workspace
    );
    if workspace.state == WorkspaceState::Preparing {
        let Some(prepared) = prepare_workspace(ctx, &workspace).await? else {
            return Ok(1);
        };
        workspace = prepared;
    }
    ctx.emit(
        &format!(
            "Created {} on branch {} at {}",
            workspace.name,
            workspace.branch,
            workspace.path.display()
        ),
        &workspace,
    )?;
    shell::navigate(&workspace.path, ctx.json)?;
    match agent {
        Some(Agent::Codex) => codex(ctx, codex_mode, Some(workspace.id), args).await,
        Some(Agent::Claude) => claude(ctx, Some(workspace.id), args).await,
        None => Ok(0),
    }
}

pub(super) async fn prepare(ctx: &Context, workspace: Option<String>) -> Result<i32> {
    let workspace = ui::select_workspace(ctx, workspace, Fallback::CurrentDirectory).await?;
    let inspection = client::inspect(&ctx.paths, workspace).await?;
    let Some(workspace) = prepare_workspace(ctx, &inspection.workspace).await? else {
        return Ok(1);
    };
    ctx.emit(&format!("Prepared {}", workspace.name), &workspace)?;
    Ok(0)
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
        if result.is_ok() || !std::env::current_dir()?.exists() {
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
/// its repository checkout, or home when that is unavailable.
async fn escape_destination(ctx: &Context, workspace: &Workspace) -> Result<Option<PathBuf>> {
    let cwd = std::env::current_dir()?;
    if !workspace.contains(&cwd) {
        return Ok(None);
    }
    let repository = client::repositories(&ctx.paths)
        .await?
        .into_iter()
        .find(|r| r.id == workspace.repository_id)
        .map(|r| r.path)
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
    let command = std::iter::once("claude".into())
        .chain(args)
        .chain(["--remote-control".into(), inspection.workspace.name.into()])
        .collect();
    execution::run(&ctx.paths, inspection.workspace.id, command).await
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
