//! CLI workspace workflows; all state mutations go through the daemon.
use std::{ffi::OsString, path::PathBuf};

use anyhow::{Context as _, Result, bail, ensure};
use serde_json::json;

use crate::{
    cli::{Agent, CodexMode},
    client::{self, request},
    context::Context,
    env, execution,
    existing_branch::{Branch, OpenedWorkspace},
    git,
    happy::{self, HappyAgent},
    hooks::{self, Hook},
    model::{DiffBase, Workspace, WorkspaceStatus},
    output::{Palette, Style},
    protocol::{ConfigTarget, Method},
    recovery::{ReconcileOptions, Report},
    removal::{BranchChoice, RemovalCheck, RemovalResult},
    repo_config::Hooks,
    shell,
    state::WorkspaceState,
    templates,
    ui::{self, Fallback},
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
    let base = request::<DiffBase>(&ctx.paths, Method::DiffBase { workspace }).await?;
    execution::run(
        &ctx.paths,
        base.workspace_id,
        vec!["git".into(), "diff".into(), base.commit.into(), "--".into()],
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
    let Creation {
        path,
        branch,
        mut existing,
        base,
        git_profile,
    } = creation;
    let path = path
        .map(|path| super::repositories::absolute(ctx, path))
        .transpose()?;
    let issue_command = matches!(agent, AgentLaunch::IssueDefault(_));
    let repository = match repository {
        Some(repo) => ui::repository_selector(repo)?,
        None => {
            let repos = client::repositories(&ctx.paths).await?;
            match issue.as_deref() {
                Some(number)
                    if issue_command
                        && !number.is_empty()
                        && number.bytes().all(|c| c.is_ascii_digit()) =>
                {
                    super::issues::repository_for_number(ctx, repos).await?
                }
                Some(url)
                    if issue_command
                        || url.starts_with("https://")
                        || url.starts_with("http://") =>
                {
                    super::issues::repository_for(&repos, url).await?.id.clone()
                }
                _ => ui::pick(ctx, "Repository> ", ui::repository_choices(repos).await?)?,
            }
        }
    };
    let agent = match agent {
        AgentLaunch::Explicit(agent) => agent,
        AgentLaunch::IssueDefault(agent) => {
            Some(default_agent(ctx, ConfigTarget::Repository(repository.clone()), agent).await?)
        }
    };
    // Validate launch configuration before creating a workspace.
    if let Some(Agent::Custom(name)) = &agent {
        let settings =
            client::settings(&ctx.paths, ConfigTarget::Repository(repository.clone())).await?;
        ensure!(
            settings.commands.contains_key(name),
            "unknown agent {name:?}; define it in [commands] in Shoal config"
        );
    }
    let codex_mode = match &agent {
        Some(Agent::Codex) if issue.is_some() => Some(CodexMode::Cli),
        Some(Agent::Codex) => Some(
            client::settings(&ctx.paths, ConfigTarget::Repository(repository.clone()))
                .await?
                .codex
                .default_mode,
        ),
        _ => None,
    };
    let issue = match issue {
        Some(input) => {
            let repos = client::repositories(&ctx.paths).await?;
            let repo = crate::repository::select(&repos, &repository).await?;
            Some(super::issues::load(repo, &input).await?)
        }
        None => None,
    };
    let mut branch = branch.or_else(|| issue.as_ref().map(|issue| issue.branch_name()));
    if branch.is_none() && existing.is_none() && base.is_none() && ctx.interactive() {
        let mode = ui::pick(
            ctx,
            "Workspace> ",
            vec![
                ("new".into(), "Create a new branch".into()),
                ("existing".into(), "Use an existing branch".into()),
            ],
        )?;
        if mode == "existing" {
            let branches = request::<Vec<Branch>>(
                &ctx.paths,
                Method::ListBranches {
                    repository: repository.clone(),
                },
            )
            .await?;
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
            existing = Some(ui::pick(ctx, "Branch> ", entries)?);
        }
    }
    let (mut workspace, reused) = if let Some(branch) = existing {
        let opened = request::<OpenedWorkspace>(
            &ctx.paths,
            Method::OpenBranch {
                path,
                repository,
                branch,
                git_profile,
                base,
            },
        )
        .await?;
        (opened.workspace, opened.reused)
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
        (
            request::<Workspace>(
                &ctx.paths,
                Method::CreateWorkspace {
                    path,
                    repository,
                    name,
                    base,
                    git_profile,
                },
            )
            .await?,
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
    let prompt = if let Some(issue) = issue.filter(|_| agent.is_some()) {
        let settings =
            client::settings(&ctx.paths, ConfigTarget::Workspace(workspace.id.clone())).await?;
        Some(issue.prompt(settings.issue_template.as_deref()))
    } else {
        None
    };
    match agent {
        Some(agent) => launch_agent(ctx, agent, codex_mode, workspace, prompt, args).await,
        None => Ok(0),
    }
}

/// The requested agent, else the configured `default_agent`, else a picker.
pub(super) async fn default_agent(
    ctx: &Context,
    target: ConfigTarget,
    agent: Option<Agent>,
) -> Result<Agent> {
    let settings = client::settings(&ctx.paths, target).await?;
    match agent.or(settings.default_agent.clone()) {
        Some(agent) => Ok(agent),
        None if ctx.interactive() => ui::pick(
            ctx,
            "Agent> ",
            Agent::possible_values()
                .into_iter()
                .chain(
                    settings
                        .commands
                        .keys()
                        .filter(|name| matches!(name.parse(), Ok(Agent::Custom(_))))
                        .cloned(),
                )
                .map(|value| (value.clone(), value))
                .collect(),
        )?
        .parse()
        .map_err(|()| anyhow::anyhow!("unknown agent")),
        None => bail!(
            "no agent selected; pass --agent or set default_agent in the repository or global config"
        ),
    }
}

/// Start an agent in a ready workspace, giving it the prompt the way it accepts one.
pub(super) async fn launch_agent(
    ctx: &Context,
    agent: Agent,
    codex_mode: Option<CodexMode>,
    workspace: Workspace,
    prompt: Option<String>,
    mut args: Vec<OsString>,
) -> Result<i32> {
    if let Some(prompt) = &prompt
        && !matches!(agent, Agent::Happy(_) | Agent::Custom(_))
    {
        args.insert(0, prompt.into());
    }
    match agent {
        Agent::Codex => codex(ctx, codex_mode, Some(workspace.id), args).await,
        Agent::Claude => claude(ctx, Some(workspace.id), args).await,
        Agent::Happy(agent) => happy(ctx, agent, Some(workspace.id), prompt, args).await,
        Agent::Custom(name) => custom_agent(ctx, &name, workspace, prompt, args).await,
    }
}

async fn custom_agent(
    ctx: &Context,
    name: &str,
    workspace: Workspace,
    prompt: Option<String>,
    mut args: Vec<OsString>,
) -> Result<i32> {
    let settings =
        client::settings(&ctx.paths, ConfigTarget::Workspace(workspace.id.clone())).await?;
    let instructions = templates::instructions(settings.agent_template.as_deref(), &workspace);
    let prompt = [instructions, prompt.unwrap_or_default()]
        .into_iter()
        .filter(|part| !part.is_empty())
        .collect::<Vec<_>>()
        .join("\n\n");
    let argv = settings
        .commands
        .get(name)
        .with_context(|| format!("unknown agent {name:?}; define it in [commands]"))?;
    if !prompt.is_empty() && !argv.iter().any(|arg| arg.contains("{prompt}")) {
        args.insert(0, prompt.clone().into());
    }
    let command = crate::named_commands::expand_with_fields(
        &ctx.paths,
        &settings.commands,
        name,
        &workspace,
        args,
        &[("{prompt}", std::ffi::OsStr::new(&prompt))],
    )
    .await?;
    execution::run(&ctx.paths, workspace.id, command, Some(name.into())).await
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
    let Some(workspace) = prepare_workspace(ctx, &inspection.workspace).await? else {
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
    let hooks = request::<Hooks>(
        &ctx.paths,
        Method::WorkspaceHooks {
            workspace: workspace.id.clone(),
        },
    )
    .await?;
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
    let workspace = &status.workspace;
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

    println!("Executions:    {}", status.executions.len());
    for execution in &status.executions {
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

    println!("Ports:         {}", status.ports.len());
    for port in &status.ports {
        println!("  {}={} ({})", port.name, port.port, port.env_var);
    }

    println!("Simulators:    {}", status.simulators.len());
    for simulator in &status.simulators {
        println!(
            "  {}={}  {}  {}  {}",
            simulator.lease_name.as_deref().unwrap_or("default"),
            simulator.udid.as_deref().unwrap_or("pending"),
            palette.simulator_state(simulator.state),
            simulator.device,
            simulator.runtime
        );
    }

    println!("Resources:     {}", status.resources.len());
    for resource in &status.resources {
        println!(
            "  {}/{} -> {} [{}]",
            resource.pool, resource.name, resource.resource, resource.mode
        );
    }

    match &status.pr_cleanup {
        Some(registration) => {
            let target = registration
                .url
                .as_deref()
                .or(registration.head.as_deref())
                .unwrap_or("registered");
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
    execution::run(&ctx.paths, workspace, command, None).await
}

pub(super) async fn claude(
    ctx: &Context,
    workspace: Option<String>,
    args: Vec<OsString>,
) -> Result<i32> {
    let workspace = ui::select_workspace(ctx, workspace, Fallback::CurrentDirectory).await?;
    let inspection = client::inspect(&ctx.paths, workspace).await?;
    let settings = client::settings(
        &ctx.paths,
        ConfigTarget::Workspace(inspection.workspace.id.clone()),
    )
    .await?;
    let instructions =
        templates::instructions(settings.agent_template.as_deref(), &inspection.workspace);
    trust_claude(ctx, &inspection.workspace.path);
    let args = templates::instruction_args(HappyAgent::Claude, instructions)
        .into_iter()
        .chain(args)
        .collect();
    let command = crate::named_commands::expand(
        &ctx.paths,
        &settings.commands,
        "claude",
        &inspection.workspace,
        args,
    )
    .await?;
    execution::run(
        &ctx.paths,
        inspection.workspace.id,
        command,
        Some("claude".into()),
    )
    .await
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

fn trust_codex(ctx: &Context, workspace: &std::path::Path) {
    let trusted = env::codex_home().and_then(|dir| {
        let config = dir
            .unwrap_or_else(|| ctx.paths.home.join(".codex"))
            .join("config.toml");
        trust_codex_workspace(&config, workspace)
    });
    if let Err(error) = trusted {
        eprintln!("warning: could not mark the workspace as trusted for Codex: {error:#}");
    }
}

pub(super) async fn codex(
    ctx: &Context,
    mode: Option<CodexMode>,
    workspace: Option<String>,
    args: Vec<OsString>,
) -> Result<i32> {
    let workspace = ui::select_workspace(ctx, workspace, Fallback::CurrentDirectory).await?;
    // Explicit desktop handoffs do not consume prompt templates.
    if mode == Some(CodexMode::App) {
        return open_app(ctx, Some(workspace), "codex", args).await;
    }
    let settings = client::settings(&ctx.paths, ConfigTarget::Workspace(workspace.clone())).await?;
    if mode.unwrap_or(settings.codex.default_mode) == CodexMode::App {
        return open_app(ctx, Some(workspace), "codex", args).await;
    }
    let inspection = client::inspect(&ctx.paths, workspace.clone()).await?;
    trust_codex(ctx, &inspection.workspace.path);
    let instructions =
        templates::instructions(settings.agent_template.as_deref(), &inspection.workspace);
    let args = templates::instruction_args(HappyAgent::Codex, instructions)
        .into_iter()
        .chain(args)
        .collect();
    let command = crate::named_commands::expand(
        &ctx.paths,
        &settings.commands,
        "codex",
        &inspection.workspace,
        args,
    )
    .await?;
    execution::run(&ctx.paths, workspace, command, Some("codex".into())).await
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
    mut args: Vec<OsString>,
) -> Result<i32> {
    let workspace = ui::select_workspace(ctx, workspace, Fallback::CurrentDirectory).await?;
    let workspace = client::inspect(&ctx.paths, workspace).await?.workspace;
    let settings =
        client::settings(&ctx.paths, ConfigTarget::Workspace(workspace.id.clone())).await?;
    let instructions = templates::instructions(settings.agent_template.as_deref(), &workspace);
    let prompt = if agent == HappyAgent::Codex && !instructions.is_empty() {
        Some(match prompt {
            Some(prompt) if !prompt.is_empty() => format!("{instructions}\n\n{prompt}"),
            _ => instructions,
        })
    } else {
        let mut configured = templates::instruction_args(HappyAgent::Claude, instructions);
        configured.append(&mut args);
        args = configured;
        prompt
    };
    match agent {
        HappyAgent::Claude => trust_claude(ctx, &workspace.path),
        HappyAgent::Codex => trust_codex(ctx, &workspace.path),
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
    let launch = match execution::launch_detached(
        &ctx.paths,
        &workspace,
        log,
        command,
        &happy::client::RECONNECT_ENV,
        &env,
        Some(&format!("happy {}", agent.name())),
    )
    .await
    {
        Ok(launch) => launch,
        Err(error) => {
            // Nothing will ever attach to the seeded session; do not leave it in the app.
            if let Some(seeded) = seeded {
                seeded.discard().await;
            }
            return Err(error);
        }
    };
    // A prompt passed as an argument is delivered by the launch itself.
    let mut prompt_delivered = prompt.is_some() && agent.accepts_prompt();
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
    if program == "codex" {
        trust_codex(ctx, &inspection.workspace.path);
    }
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
/// file changed, creating it when needed.
fn trust_claude_workspace(config: &std::path::Path, workspace: &std::path::Path) -> Result<bool> {
    use serde_json::Value;
    let workspace = std::fs::canonicalize(workspace).unwrap_or_else(|_| workspace.to_owned());
    let key = workspace
        .to_str()
        .context("workspace path is not UTF-8")?
        .to_owned();
    let _lock = lock_trust_config(config)?;
    let text = match std::fs::read_to_string(config) {
        Ok(text) => text,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => "{}".into(),
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
    std::fs::create_dir_all(directory)?;
    let mut temporary = tempfile::NamedTempFile::new_in(directory)?;
    serde_json::to_writer_pretty(&mut temporary, &root)?;
    if let Ok(metadata) = std::fs::metadata(config) {
        temporary
            .as_file()
            .set_permissions(metadata.permissions())?;
    }
    temporary
        .persist(config)
        .with_context(|| format!("replace {}", config.display()))?;
    Ok(true)
}

fn trust_codex_workspace(config: &std::path::Path, workspace: &std::path::Path) -> Result<bool> {
    use std::io::Write;
    use toml_edit::{DocumentMut, Item, Table, value};

    let workspace = std::fs::canonicalize(workspace)?;
    let key = workspace.to_str().context("workspace path is not UTF-8")?;
    // Follow an existing config symlink instead of replacing it.
    let config = match std::fs::canonicalize(config) {
        Ok(path) => path,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => config.to_owned(),
        Err(error) => return Err(error).with_context(|| format!("resolve {}", config.display())),
    };
    let directory = config.parent().context("Codex config has no parent")?;
    let _lock = lock_trust_config(&config)?;
    let text = match std::fs::read_to_string(&config) {
        Ok(text) => text,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => String::new(),
        Err(error) => return Err(error).with_context(|| format!("read {}", config.display())),
    };
    let mut root: DocumentMut = text
        .parse()
        .with_context(|| format!("parse {}", config.display()))?;
    let project = root
        .entry("projects")
        .or_insert_with(|| Item::Table(Table::new()))
        .as_table_like_mut()
        .context("Codex config `projects` is not a TOML table")?
        .entry(key)
        .or_insert_with(|| Item::Table(Table::new()))
        .as_table_like_mut()
        .context("Codex project entry is not a TOML table")?;
    if project.get("trust_level").and_then(Item::as_str) == Some("trusted") {
        return Ok(false);
    }
    project.insert("trust_level", value("trusted"));
    let mut temporary = tempfile::NamedTempFile::new_in(directory)?;
    temporary.write_all(root.to_string().as_bytes())?;
    if let Ok(metadata) = std::fs::metadata(&config) {
        temporary
            .as_file()
            .set_permissions(metadata.permissions())?;
    }
    temporary
        .persist(&config)
        .with_context(|| format!("replace {}", config.display()))?;
    Ok(true)
}

// This lock coordinates Shoal processes; the agents do not use it themselves.
fn lock_trust_config(config: &std::path::Path) -> Result<std::fs::File> {
    use std::os::unix::fs::OpenOptionsExt;

    std::fs::create_dir_all(config.parent().context("agent config has no parent")?)?;
    let mut path = config.as_os_str().to_os_string();
    path.push(".shoal-lock");
    let lock = std::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(false)
        .mode(0o600)
        .open(path)?;
    fs2::FileExt::lock_exclusive(&lock)?;
    Ok(lock)
}

pub(super) async fn pr(
    ctx: &Context,
    workspace: Option<String>,
    url: Option<String>,
    clear: bool,
) -> Result<i32> {
    let workspace = ui::select_workspace(ctx, workspace, Fallback::CurrentDirectory).await?;
    // An acknowledged worktree disappears shortly after the call returns, so
    // leave it now, unless the sweep will retain it as dirty. A watched PR
    // keeps the shell where it is until it merges.
    let escape = if url.is_none() && !clear && !env::is_scoped() {
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
    request::<()>(
        &ctx.paths,
        Method::SetPr {
            workspace,
            url,
            clear,
        },
    )
    .await?;
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

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::Value;
    use std::fs;

    #[test]
    fn concurrent_trust_updates_keep_every_workspace() {
        for claude in [false, true] {
            let dir = tempfile::tempdir().unwrap();
            let config = dir.path().join("codex/config.toml");
            let workspaces: Vec<_> = (0..8)
                .map(|index| {
                    let path = dir.path().join(format!("workspace-{index}"));
                    fs::create_dir(&path).unwrap();
                    fs::canonicalize(path).unwrap()
                })
                .collect();
            let barrier = std::sync::Barrier::new(workspaces.len());
            std::thread::scope(|scope| {
                for path in &workspaces {
                    scope.spawn(|| {
                        barrier.wait();
                        if claude {
                            trust_claude_workspace(&config, path).unwrap();
                        } else {
                            trust_codex_workspace(&config, path).unwrap();
                        }
                    });
                }
            });
            let text = fs::read_to_string(config).unwrap();
            let root: Value = if claude {
                serde_json::from_str(&text).unwrap()
            } else {
                serde_json::to_value(toml::from_str::<toml::Value>(&text).unwrap()).unwrap()
            };
            for path in workspaces {
                let project = &root["projects"][path.to_str().unwrap()];
                if claude {
                    assert_eq!(project["hasTrustDialogAccepted"], true);
                } else {
                    assert_eq!(project["trust_level"], "trusted");
                }
            }
        }
    }

    #[test]
    fn codex_trust_preserves_settings_comments_and_config_symlinks() {
        let dir = tempfile::tempdir().unwrap();
        let config = dir.path().join("config.toml");
        let link = dir.path().join("linked.toml");
        let workspace = dir.path().join("ws.with 'quotes' and \"quotes\"");
        fs::create_dir(&workspace).unwrap();
        let key = fs::canonicalize(&workspace).unwrap();
        let key = key.to_str().unwrap();
        // An existing inline entry exercises table-like access and quoted keys.
        let original = format!(
            "# Keep my settings\nmodel = 'custom'\n[projects]\n'/other' = {{ trust_level = 'untrusted' }}\n{} = {{ trust_level = 'untrusted', custom = 42 }}\n",
            toml_edit::Key::new(key)
        );
        fs::write(&config, &original).unwrap();
        std::os::unix::fs::symlink(&config, &link).unwrap();
        assert!(trust_codex_workspace(&link, &workspace).unwrap());
        assert!(link.is_symlink());
        let text = fs::read_to_string(&config).unwrap();
        assert!(text.starts_with("# Keep my settings\nmodel = 'custom'\n"));
        let root: toml::Value = toml::from_str(&text).unwrap();
        assert_eq!(
            root["projects"][key]["trust_level"].as_str(),
            Some("trusted")
        );
        assert_eq!(root["projects"][key]["custom"].as_integer(), Some(42));
        assert_eq!(
            root["projects"]["/other"]["trust_level"].as_str(),
            Some("untrusted")
        );
        assert!(!trust_codex_workspace(&config, &workspace).unwrap());
        assert_eq!(fs::read_to_string(&config).unwrap(), text);
        for bad in [
            "invalid TOML".to_owned(),
            "projects = []".to_owned(),
            format!("[projects]\n{} = 1", toml_edit::Key::new(key)),
        ] {
            fs::write(&config, &bad).unwrap();
            assert!(trust_codex_workspace(&config, &workspace).is_err());
            assert_eq!(fs::read_to_string(&config).unwrap(), bad);
        }
    }

    #[test]
    fn claude_trust_adds_the_project_once_and_preserves_other_settings() {
        let dir = tempfile::tempdir().unwrap();
        let config = dir.path().join("config/.claude.json");
        let workspace = dir.path().join("ws");
        fs::create_dir(&workspace).unwrap();
        assert!(trust_claude_workspace(&config, &workspace).unwrap());
        let root: Value = serde_json::from_str(&fs::read_to_string(&config).unwrap()).unwrap();
        let key = fs::canonicalize(&workspace).unwrap();
        assert_eq!(
            root["projects"][key.to_str().unwrap()]["hasTrustDialogAccepted"],
            true
        );
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
