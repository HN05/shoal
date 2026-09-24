//! CLI workspace workflows; all state mutations go through the daemon.
use crate::tools::Tool;
use std::{ffi::OsString, path::PathBuf};

use anyhow::{Result, ensure};
use serde_json::json;

use crate::{
    cli::{
        Agent, CodexMode, agents,
        client::{self, request},
        context::Context,
        output::{Palette, Style},
        ui::{self, Fallback},
    },
    daemon::recovery::{ReconcileOptions, Report},
    env, execution,
    forge::{
        IssueInput,
        pr::{Action, RegistrationKind},
    },
    git::{
        self,
        existing_branch::{Branch, OpenedWorkspace},
    },
    hooks::{self, Hook, HookKind},
    model::{DiffBase, Repository, Workspace, WorkspaceStatus},
    protocol::{ConfigTarget, Method},
    removal::{BranchChoice, RemovalCheck, RemovalResult},
    shell,
    state::WorkspaceState,
};

pub(super) async fn land(ctx: &Context, workspace: Option<String>) -> Result<i32> {
    let workspace = ui::select_workspace(ctx, workspace, Fallback::CurrentDirectory).await?;
    execution::land(&ctx.paths, workspace, ctx.json).await
}

pub(super) async fn land_worker(ctx: &Context, plan: String) -> Result<i32> {
    anyhow::ensure!(env::is_scoped(), "land worker requires a tracked execution");
    request::<()>(&ctx.paths, Method::CheckLanding).await?;
    let plan: crate::model::LandPlan = serde_json::from_str(&plan)?;
    anyhow::ensure!(
        std::env::var(env::WORKSPACE_ID)? == plan.workspace.id,
        "land worker requires its authorized workspace"
    );
    let result = crate::git::repo::finish_land(plan).await?;
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
    let base = request::<DiffBase>(&ctx.paths, Method::DiffBase { workspace }).await?;
    execution::run(
        &ctx.paths,
        base.workspace_id,
        vec![
            Tool::Git.program().into(),
            "diff".into(),
            base.commit.into(),
            "--".into(),
        ],
        None,
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

pub(super) struct Creation {
    pub path: Option<PathBuf>,
    pub branch: Option<String>,
    pub existing: Option<String>,
    pub base: Option<String>,
    pub git_profile: Option<String>,
}

pub(super) enum AgentLaunch {
    Explicit(Option<Agent>),
    IssueDefault(Option<Agent>),
}

pub(super) async fn add(
    ctx: &Context,
    repository: Option<String>,
    creation: Creation,
    issue: Option<String>,
    agent: AgentLaunch,
    args: Vec<OsString>,
) -> Result<i32> {
    let mut creation = creation;
    creation.path = creation
        .path
        .map(|path| super::repositories::absolute(ctx, path))
        .transpose()?;
    let target = resolve_add_target(ctx, repository, issue.as_deref(), &agent).await?;
    let agent = resolve_add_agent(ctx, &target.selector, agent, issue.is_some()).await?;
    let issue = match issue {
        Some(input) => Some(super::issues::load(target.repository(ctx).await?, &input).await?),
        None => None,
    };
    let opened = open_add_workspace(ctx, &target, creation, issue.as_ref()).await?;
    let Some(workspace) = finish_add_workspace(ctx, opened).await? else {
        return Ok(1);
    };
    agent.launch(ctx, workspace, issue, args).await
}

struct AddTarget {
    selector: String,
    repositories: tokio::sync::OnceCell<Vec<Repository>>,
}

impl AddTarget {
    async fn repository(&self, ctx: &Context) -> Result<&Repository> {
        let repos = self
            .repositories
            .get_or_try_init(|| client::repositories(&ctx.paths))
            .await?;
        crate::forge::repository::select(repos, &self.selector).await
    }
}

async fn resolve_add_target(
    ctx: &Context,
    repository: Option<String>,
    issue: Option<&str>,
    agent: &AgentLaunch,
) -> Result<AddTarget> {
    // Load on first use so explicit targets keep their original failure order.
    let repositories = tokio::sync::OnceCell::new();
    let selector = match repository {
        Some(repo) => ui::repository_selector(repo)?,
        None => {
            let repos = repositories
                .get_or_try_init(|| client::repositories(&ctx.paths))
                .await?;
            let issue_command = matches!(agent, AgentLaunch::IssueDefault(_));
            match issue.map(|input| (input, IssueInput::parse(input))) {
                Some((_, IssueInput::Number)) if issue_command => {
                    super::issues::repository_for_number(ctx, repos.clone()).await?
                }
                Some((url, kind)) if issue_command || kind == IssueInput::Url => {
                    super::issues::repository_for(repos, url).await?.id.clone()
                }
                _ => ui::pick(
                    ctx,
                    "Repository> ",
                    ui::repository_choices(repos.clone()).await?,
                )?,
            }
        }
    };
    Ok(AddTarget {
        selector,
        repositories,
    })
}

struct ResolvedAddAgent {
    agent: Option<Agent>,
    codex_mode: Option<CodexMode>,
}

async fn resolve_add_agent(
    ctx: &Context,
    repository: &str,
    agent: AgentLaunch,
    has_issue: bool,
) -> Result<ResolvedAddAgent> {
    let settings = tokio::sync::OnceCell::new();
    let load_settings =
        || client::settings(&ctx.paths, ConfigTarget::Repository(repository.into()));
    let agent = match agent {
        AgentLaunch::Explicit(agent) => agent,
        AgentLaunch::IssueDefault(agent) => Some(agents::select_default_agent(
            ctx,
            settings.get_or_try_init(load_settings).await?,
            agent,
        )?),
    };
    // Validate launch configuration before looking up the issue or creating work.
    if let Some(Agent::Custom(name)) = &agent {
        ensure!(
            settings
                .get_or_try_init(load_settings)
                .await?
                .commands
                .contains_key(name),
            "unknown agent {name:?}; define it in [commands] in Shoal config"
        );
    }
    let codex_mode = match &agent {
        Some(Agent::Codex) if has_issue => Some(CodexMode::Cli),
        Some(Agent::Codex) => Some(
            settings
                .get_or_try_init(load_settings)
                .await?
                .codex
                .default_mode,
        ),
        _ => None,
    };
    Ok(ResolvedAddAgent { agent, codex_mode })
}

async fn open_add_workspace(
    ctx: &Context,
    target: &AddTarget,
    creation: Creation,
    issue: Option<&super::issues::Issue>,
) -> Result<OpenedWorkspace> {
    let Creation {
        path,
        branch,
        mut existing,
        base,
        git_profile,
    } = creation;
    let repository = target.selector.clone();
    let mut branch = branch.or_else(|| issue.map(|issue| issue.branch_name()));
    if branch.is_none() && existing.is_none() && base.is_none() && ctx.interactive() {
        #[derive(Clone, Copy)]
        enum BranchMode {
            New,
            Existing,
        }
        let mode = ui::pick_choice(
            ctx,
            "Workspace> ",
            &[
                (BranchMode::New, "Create a new branch"),
                (BranchMode::Existing, "Use an existing branch"),
            ],
        )?;
        if let BranchMode::Existing = mode {
            existing = Some(pick_add_branch(ctx, target).await?);
        }
    }
    if let Some(branch) = existing {
        request::<OpenedWorkspace>(
            &ctx.paths,
            Method::OpenBranch {
                path,
                repository,
                branch,
                git_profile,
                base,
            },
        )
        .await
    } else {
        let name = match branch.take() {
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
        let workspace = request::<Workspace>(
            &ctx.paths,
            Method::CreateWorkspace {
                path,
                repository,
                name,
                base,
                git_profile,
            },
        )
        .await?;
        Ok(OpenedWorkspace {
            workspace,
            reused: false,
        })
    }
}

async fn pick_add_branch(ctx: &Context, target: &AddTarget) -> Result<String> {
    let branches = request::<Vec<Branch>>(
        &ctx.paths,
        Method::ListBranches {
            repository: target.selector.clone(),
        },
    )
    .await?;
    let workspaces = client::workspaces(&ctx.paths).await?;
    let repo = target.repository(ctx).await?;
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
    ui::pick(ctx, "Branch> ", entries)
}

async fn finish_add_workspace(ctx: &Context, opened: OpenedWorkspace) -> Result<Option<Workspace>> {
    let OpenedWorkspace {
        mut workspace,
        reused,
    } = opened;
    if workspace.state == WorkspaceState::Preparing {
        let Some(ready) = setup_workspace(ctx, &workspace).await? else {
            return Ok(None);
        };
        workspace = ready;
    }
    ctx.emit(
        &format!(
            "{} {} on branch {} at {}",
            if reused { "Opened" } else { "Created" },
            Palette::stdout(ctx.json).paint(Style::Heading, &workspace.name),
            workspace.branch,
            workspace.path.display()
        ),
        &workspace,
    )?;
    shell::navigate(&workspace.path, ctx.json)?;
    if !reused {
        run_post_setup(ctx, &workspace).await?;
    }
    Ok(Some(workspace))
}

impl ResolvedAddAgent {
    async fn launch(
        self,
        ctx: &Context,
        workspace: Workspace,
        issue: Option<super::issues::Issue>,
        args: Vec<OsString>,
    ) -> Result<i32> {
        let Some(agent) = self.agent else {
            return Ok(0);
        };
        // Use the ready worktree's settings: setup may have changed its template.
        let prompt = if let Some(issue) = issue {
            let settings =
                client::settings(&ctx.paths, ConfigTarget::Workspace(workspace.id.clone())).await?;
            Some(issue.prompt(settings.issue_template.as_deref()))
        } else {
            None
        };
        agents::launch_agent(ctx, agent, self.codex_mode, workspace, prompt, args).await
    }
}

pub(super) async fn adopt(ctx: &Context, repository: String, path: PathBuf) -> Result<i32> {
    let repository = ui::repository_selector(repository)?;
    let path = super::repositories::absolute(ctx, path)?;
    let workspace =
        request::<Workspace>(&ctx.paths, Method::AdoptWorkspace { repository, path }).await?;
    ctx.emit(
        &format!(
            "Adopted {} on branch {} at {} (normal cleanup applies)",
            Palette::stdout(ctx.json).paint(Style::Heading, &workspace.name),
            workspace.branch,
            workspace.path.display()
        ),
        &workspace,
    )?;
    shell::navigate(&workspace.path, ctx.json)?;
    Ok(0)
}

pub(super) async fn setup(ctx: &Context, workspace: Option<String>) -> Result<i32> {
    let workspace = ui::select_workspace(ctx, workspace, Fallback::CurrentDirectory).await?;
    let inspection = client::inspect(&ctx.paths, workspace).await?;
    let Some(workspace) = setup_workspace(ctx, &inspection.workspace).await? else {
        return Ok(1);
    };
    run_post_setup(ctx, &workspace).await?;
    ctx.emit_styled(
        Style::Success,
        &format!("Set up {}", workspace.name),
        &workspace,
    )?;
    Ok(0)
}

/// Run the repository's `post_setup_cmd`, if any, once the workspace is ready.
/// A failure keeps the ready workspace and stops what would follow.
async fn run_post_setup(ctx: &Context, workspace: &Workspace) -> Result<()> {
    let command = request::<Option<PathBuf>>(
        &ctx.paths,
        Method::WorkspaceHook {
            workspace: workspace.id.clone(),
            kind: HookKind::PostSetup,
        },
    )
    .await?;
    if let Some(command) = command {
        hooks::run_interactive(Hook::PostSetup, workspace, &command, &ctx.paths, ctx.json).await?;
    }
    Ok(())
}

/// Run the setup command, then let an interactive user decide what to do with
/// a failed workspace. `None` means the workspace was deleted or kept as-is.
async fn setup_workspace(ctx: &Context, workspace: &Workspace) -> Result<Option<Workspace>> {
    let failure = match execution::setup(&ctx.paths, workspace.id.clone(), ctx.json).await {
        Ok(0) => None,
        Ok(code) => Some(format!("setup command exited with status {code}")),
        Err(error) => Some(format!("{error:#}")),
    };
    if let Some(error) = failure {
        let name = &workspace.name;
        ensure!(
            ctx.interactive(),
            "setup failed for {name}: {error}; workspace retained. Retry with `shoal setup {name}`, ignore with `shoal doctor {name} --repair`, or delete with `shoal rm {name} --yes --delete-branch`"
        );
        eprintln!(
            "{} for {name}: {error}",
            Palette::stderr(ctx.json).paint(Style::Error, "Setup failed")
        );
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
        request::<RemovalResult>(
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
    let reports = request::<Vec<Report>>(
        &ctx.paths,
        Method::Doctor {
            workspace: Some(workspace.id.clone()),
            options: ReconcileOptions {
                repair: true,
                ..Default::default()
            },
        },
    )
    .await?;
    ensure!(
        reports
            .iter()
            .all(|r| r.workspace.state == WorkspaceState::Ready),
        "workspace still has unresolved ownership or processes; inspect with shoal doctor"
    );
    Ok(())
}

pub(super) async fn list(ctx: &Context) -> Result<i32> {
    let workspaces = client::workspaces(&ctx.paths).await?;
    ctx.show(&workspaces, |workspaces| {
        let palette = Palette::stdout(ctx.json);
        for workspace in workspaces {
            println!("{}", ui::workspace_label(workspace, palette));
        }
    })?;
    // A pointer for humans; agents cannot read notifications and JSON stays clean.
    if !ctx.json
        && !env::is_scoped()
        && let Ok(Some(status)) = client::status(&ctx.paths).await
        && status.unread_notifications > 0
    {
        eprintln!(
            "{} new notification{}; run shoal notifications",
            status.unread_notifications,
            if status.unread_notifications == 1 {
                ""
            } else {
                "s"
            }
        );
    }
    Ok(0)
}

pub(super) async fn status(ctx: &Context, workspace: Option<String>) -> Result<i32> {
    let workspace = ui::select_workspace(ctx, workspace, Fallback::CurrentDirectory).await?;
    let status =
        request::<WorkspaceStatus>(&ctx.paths, Method::WorkspaceStatus { workspace }).await?;
    ctx.show(&status, |status| render_status(status, ctx.json))?;
    Ok(0)
}

fn render_status(status: &WorkspaceStatus, json: bool) {
    let palette = Palette::stdout(json);
    let inspection = &status.inspection;
    let workspace = &inspection.workspace;
    println!(
        "{}  {}",
        palette.paint(Style::Heading, &workspace.name),
        palette.workspace_state(workspace.state)
    );
    println!("Path:          {}", workspace.path.display());
    println!("Branch:        {}", workspace.branch);
    println!(
        "Setup:         {}",
        if status.setup_finished {
            "finished"
        } else {
            "in progress"
        }
    );
    match &status.diff {
        Some(diff) => println!(
            "Changes:       {} file{}, +{} -{}",
            diff.files_changed,
            if diff.files_changed == 1 { "" } else { "s" },
            diff.insertions,
            diff.deletions
        ),
        None => {
            println!("Changes:       unavailable");
            if let Some(error) = &status.diff_error {
                println!("  {}", palette.paint(Style::Warning, error));
            }
        }
    }
    if let Some(error) = &workspace.error {
        println!("Error:         {}", palette.paint(Style::Error, error));
    }

    println!("Executions:    {}", inspection.executions.len());
    for execution in &inspection.executions {
        let pid = execution
            .child
            .as_ref()
            .or(execution.wrapper.as_ref())
            .map(|identity| format!("  pid {}", identity.pid))
            .unwrap_or_default();
        println!(
            "  {}  {}{}",
            execution.id,
            palette.execution_state(execution.state),
            pid
        );
    }

    println!("Ports:         {}", inspection.ports.len());
    for port in &inspection.ports {
        println!("  {}={} ({})", port.name, port.port, port.env_var);
    }

    println!("Simulators:    {}", inspection.simulators.len());
    for simulator in &inspection.simulators {
        println!(
            "  {}={}  {}  {}  {}",
            simulator.lease_name.as_deref().unwrap_or("default"),
            simulator.udid.as_deref().unwrap_or("pending"),
            palette.simulator_state(simulator.state),
            simulator.device,
            simulator.runtime
        );
    }

    println!("Resources:     {}", inspection.resources.len());
    for resource in &inspection.resources {
        println!(
            "  {}/{} -> {} [{}]",
            resource.pool, resource.name, resource.resource, resource.mode
        );
    }

    match &inspection.pr_cleanup {
        Some(registration) => {
            let target = match &registration.kind {
                RegistrationKind::Watch { url } => url,
                RegistrationKind::Acknowledgement { head } => head,
            };
            println!("PR watch:      {target}");
            if let Some(error) = &registration.error {
                println!("  {}", palette.paint(Style::Warning, error));
            }
        }
        None => println!("PR watch:      none"),
    }
    println!("Notifications: {} unread", status.unread_notifications);
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
    request::<()>(&ctx.paths, Method::StopWorkspace { workspace }).await?;
    ctx.emit_styled(
        Style::Success,
        "Workspace processes stopped",
        json!({"stopped": true}),
    )?;
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
    let check = request::<RemovalCheck>(
        &ctx.paths,
        Method::CheckRemoval {
            workspace: workspace.clone(),
            caller_pid,
        },
    )
    .await?;
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
    let result = request::<RemovalResult>(
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
    let result = result?;
    if let Some(error) = &result.hook_error {
        eprintln!("warning: {error}");
    }
    ctx.emit_styled(Style::Success, &removal_message(&result), &result)?;
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
    match (&result.branch, result.branch_outcome.is_deleted()) {
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
    execution::run(&ctx.paths, workspace, command, None).await
}

pub(super) async fn pr(ctx: &Context, workspace: Option<String>, action: Action) -> Result<i32> {
    let workspace = ui::select_workspace(ctx, workspace, Fallback::CurrentDirectory).await?;
    let clear = matches!(action, Action::Clear);
    // An acknowledged worktree disappears shortly after the call returns, so
    // leave it now, unless the sweep will retain it as dirty. A watched PR
    // keeps the shell where it is until it merges.
    let escape = if matches!(action, Action::Acknowledge) && !env::is_scoped() {
        let check = request::<RemovalCheck>(
            &ctx.paths,
            Method::CheckRemoval {
                workspace: workspace.clone(),
                caller_pid: std::process::id(),
            },
        )
        .await?;
        if check.dirty {
            None
        } else {
            escape_destination(ctx, &check.workspace).await?
        }
    } else {
        None
    };
    request::<()>(&ctx.paths, Method::SetPr { workspace, action }).await?;
    if let Some(destination) = escape {
        shell::navigate(&destination, ctx.json)?;
    }
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
