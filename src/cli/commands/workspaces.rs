//! CLI workspace workflows; all state mutations go through the daemon.
use crate::tools::Tool;
use std::{
    ffi::OsString,
    path::{Path, PathBuf},
};

use anyhow::{Context as _, Result, ensure};
use serde_json::json;

use crate::{
    agent::{Agent, CodexMode},
    cli::{
        agents,
        client::{self, request},
        context::Context,
        output::{Palette, Style},
        ui::{self, Fallback},
    },
    daemon::recovery::{ReconcileOptions, Report},
    env, execution,
    forge::{
        IssueInput,
        link::Selection,
        pr::{RegistrationKind, state::PrStatus},
        repository,
    },
    git::{
        self,
        existing_branch::{Branch, OpenedWorkspace},
    },
    hooks::{self, Hook, HookKind},
    model::{Completion, DiffBase, Repository, Workspace, WorkspaceStatus, WorkspaceTarget},
    protocol::{ConfigTarget, Method},
    removal::{BranchChoice, RemovalCheck, RemovalResult},
    shell,
    state::WorkspaceState,
};

pub(super) async fn environment(
    ctx: &Context,
    workspace: Option<String>,
    revoke: Option<String>,
) -> Result<i32> {
    let workspace = ui::select_workspace(ctx, workspace, Fallback::CurrentDirectory).await?;
    if let Some(token) = revoke {
        request::<()>(&ctx.paths, Method::RevokeWorkspaceEnv { workspace, token }).await?;
        ctx.emit_styled(
            Style::Success,
            "Workspace environment revoked",
            json!({"revoked": true}),
        )?;
    } else {
        let values = request::<std::collections::BTreeMap<String, String>>(
            &ctx.paths,
            Method::WorkspaceEnv { workspace },
        )
        .await?;
        ctx.show(&values, |values| {
            for (name, value) in values {
                println!("{name}={value}");
            }
        })?;
    }
    Ok(0)
}

pub(super) async fn land(ctx: &Context, workspace: Option<String>) -> Result<i32> {
    let workspace = ui::select_workspace(ctx, workspace, Fallback::CurrentDirectory).await?;
    execution::land(&ctx.paths, workspace, ctx.json).await
}

pub(super) async fn rename(
    ctx: &Context,
    workspace: Option<String>,
    branch: String,
) -> Result<i32> {
    let workspace = ui::select_workspace(ctx, workspace, Fallback::CurrentDirectory).await?;
    let renamed =
        request::<Workspace>(&ctx.paths, Method::RenameWorkspace { workspace, branch }).await?;
    ctx.show(&renamed, |workspace| {
        println!(
            "Renamed workspace to {} ({})",
            workspace.name, workspace.branch
        )
    })?;
    Ok(0)
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
        if env::inherits_scope(&ctx.paths.state) {
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

#[derive(serde::Serialize, serde::Deserialize)]
pub(in crate::cli) struct Creation {
    pub path: Option<PathBuf>,
    pub branch: Option<String>,
    pub existing: Option<String>,
    pub base: Option<String>,
    pub git_profile: Option<String>,
}

pub(super) enum AgentLaunch {
    Explicit(Option<Agent>),
    /// An ordinary add uses the interactive picker when no agent is named.
    Add(Option<Agent>),
    IssueDefault(Option<Agent>),
}

pub(super) async fn add(
    ctx: &Context,
    repository: Option<String>,
    creation: Creation,
    issue: Option<String>,
    agent: AgentLaunch,
    args: Vec<OsString>,
    here: bool,
) -> Result<i32> {
    let mut creation = creation;
    creation.path = creation
        .path
        .map(|path| super::repositories::absolute(ctx, path))
        .transpose()?;
    let target = resolve_add_target(ctx, repository, issue.as_deref(), &agent).await?;
    let settings = AddSettings {
        ctx,
        repository: &target.selector,
        effective: tokio::sync::OnceCell::new(),
    };
    let agent = choose_add_agent(&settings, agent).await?;
    let picked = match issue {
        Some(_) => None,
        None => pick_add_source(ctx, &target, &mut creation).await?,
    };
    // A picked issue defaults its agent as --issue does.
    let agent = match (agent, &picked) {
        (AgentChoice::PickerIfInteractive, Some(_)) => {
            choose_add_agent(&settings, AgentLaunch::IssueDefault(None)).await?
        }
        (agent, _) => agent,
    };
    // Look the issue up before any other picker or handoff, so its failures end here.
    let issue = match issue.or(picked) {
        Some(input) => Some(super::issues::load(target.repository(ctx).await?, &input).await?),
        None => None,
    };
    let creation = select_add_creation(ctx, &target, creation, issue.as_ref()).await?;
    let agent = resolve_add_agent(ctx, &settings, agent, issue.is_some()).await?;
    let tab_label = match &issue {
        Some(issue) => Some(issue.tab_label(target.repository(ctx).await?)),
        None => None,
    };
    let mut plan = AddPlan {
        repository: target.selector.clone(),
        creation,
        tab_label,
        tab_name: None,
        issue: issue.as_ref().map(|issue| issue.url.clone()),
        agent,
        args,
    };
    if crate::cli::herdr::handoff(ctx, &mut plan, issue.as_ref(), here).await? {
        return Ok(0);
    }
    execute_add_with_target(ctx, plan, target, issue).await
}

#[derive(serde::Serialize, serde::Deserialize)]
pub(in crate::cli) struct AddPlan {
    pub repository: crate::forge::repository::Selector,
    pub creation: Creation,
    tab_label: Option<String>,
    #[serde(default)]
    pub tab_name: Option<crate::cli::herdr::TabName>,
    /// The found issue's URL; its body can exceed what a Herdr tab's
    /// environment carries, so the worker looks it up again.
    issue: Option<String>,
    agent: ResolvedAddAgent,
    args: Vec<OsString>,
}

impl AddPlan {
    pub fn launches_agent(&self) -> bool {
        self.agent.agent.is_some()
    }

    pub fn label(&self) -> String {
        let branch = self
            .creation
            .branch
            .as_deref()
            .or(self.creation.existing.as_deref())
            .unwrap_or("shoal add");
        if let Some(name) = &self.tab_name {
            return name.render(branch);
        }
        self.tab_label.as_deref().unwrap_or(branch).to_owned()
    }
}

pub(in crate::cli) async fn execute_add(ctx: &Context, plan: AddPlan) -> Result<i32> {
    let target = AddTarget {
        selector: plan.repository.clone(),
        repositories: tokio::sync::OnceCell::new(),
    };
    let issue = match &plan.issue {
        Some(url) => Some(super::issues::load(target.repository(ctx).await?, url).await?),
        None => None,
    };
    execute_add_with_target(ctx, plan, target, issue).await
}

async fn execute_add_with_target(
    ctx: &Context,
    plan: AddPlan,
    target: AddTarget,
    issue: Option<super::issues::Issue>,
) -> Result<i32> {
    let AddPlan {
        repository: _,
        creation,
        tab_label: _,
        tab_name,
        issue: _,
        agent,
        args,
    } = plan;
    let opened = open_add_workspace(ctx, &target, creation).await?;
    if let Some(tab) = &ctx.herdr_tab {
        tab.watch_workspace(ctx, &opened.workspace.id)?;
        let label = match tab_name {
            Some(name) => name.render(&opened.workspace.branch),
            None => match &issue {
                Some(issue) => issue.tab_label(target.repository(ctx).await?),
                None => opened.workspace.branch.clone(),
            },
        };
        tab.rename(&label).await;
    }
    if let Some(issue) = &issue {
        request::<()>(
            &ctx.paths,
            Method::SetIssue {
                workspace: opened.workspace.id.clone(),
                url: issue.url.clone(),
            },
        )
        .await?;
    }
    let Some(workspace) = finish_add_workspace(ctx, opened).await? else {
        return Ok(1);
    };
    agent.launch(ctx, workspace, issue, args).await
}

struct AddTarget {
    selector: crate::forge::repository::Selector,
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
    let mut repositories = None;
    let selector = match repository {
        Some(repo) => {
            let selector = ui::repository_selector(repo)?;
            super::repositories::offer_unregistered(ctx, &selector)
                .await?
                .map_or(selector, |repo| repo.id.into())
        }
        None => {
            let repos = repositories.insert(client::repositories(&ctx.paths).await?);
            let issue_command = matches!(agent, AgentLaunch::IssueDefault(_));
            match issue.map(|input| (input, IssueInput::parse(input))) {
                Some((_, IssueInput::Number)) if issue_command => {
                    super::issues::current_repository(
                        ctx,
                        repos.clone(),
                        super::issues::REPO_OR_URL,
                    )
                    .await?
                    .into()
                }
                Some((url, kind)) if issue_command || kind == IssueInput::Url => {
                    super::issues::repository_for(ctx, repos, url).await?.into()
                }
                _ => ui::pick(
                    ctx,
                    "Repository> ",
                    ui::repository_choices(repos.clone()).await?,
                )?
                .into(),
            }
        }
    };
    Ok(AddTarget {
        selector,
        repositories: tokio::sync::OnceCell::new_with(repositories),
    })
}

#[derive(serde::Serialize, serde::Deserialize)]
struct ResolvedAddAgent {
    agent: Option<Agent>,
    codex_mode: Option<CodexMode>,
}

/// The repository's effective settings, loaded on first use.
struct AddSettings<'a> {
    ctx: &'a Context,
    repository: &'a crate::forge::repository::Selector,
    effective: tokio::sync::OnceCell<crate::config::Effective>,
}

impl AddSettings<'_> {
    async fn get(&self) -> Result<&crate::config::Effective> {
        self.effective
            .get_or_try_init(|| {
                client::settings(
                    &self.ctx.paths,
                    ConfigTarget::Repository(self.repository.into()),
                )
            })
            .await
    }
}

enum AgentChoice {
    Chosen(Option<Agent>),
    /// No agent was named or configured; the picker runs after lookups.
    Picker,
    /// Ordinary additions pick only when attached to a terminal.
    PickerIfInteractive,
}

/// Validate a named agent before looking up the issue or creating work.
async fn choose_add_agent(settings: &AddSettings<'_>, agent: AgentLaunch) -> Result<AgentChoice> {
    let agent = match agent {
        AgentLaunch::Explicit(agent) => agent,
        AgentLaunch::Add(agent) => {
            if agent.is_none() {
                return Ok(AgentChoice::PickerIfInteractive);
            }
            agent
        }
        AgentLaunch::IssueDefault(agent) => {
            match agent.or(settings.get().await?.default_agent.clone()) {
                Some(agent) => Some(agent),
                None => return Ok(AgentChoice::Picker),
            }
        }
    };
    if let Some(agent) = &agent {
        agents::ensure_installed(agent, &settings.get().await?.commands)?;
    }
    Ok(AgentChoice::Chosen(agent))
}

async fn resolve_add_agent(
    ctx: &Context,
    settings: &AddSettings<'_>,
    agent: AgentChoice,
    has_issue: bool,
) -> Result<ResolvedAddAgent> {
    let agent = match agent {
        AgentChoice::Chosen(agent) => agent,
        AgentChoice::Picker => agents::pick_default_agent(ctx, settings.get().await?)?,
        AgentChoice::PickerIfInteractive if ctx.interactive() => {
            agents::pick_default_agent(ctx, settings.get().await?)?
        }
        AgentChoice::PickerIfInteractive => None,
    };
    let codex_mode = match &agent {
        Some(Agent::Codex) if has_issue => Some(CodexMode::Cli),
        Some(Agent::Codex) => Some(settings.get().await?.codex.default_mode),
        _ => None,
    };
    Ok(ResolvedAddAgent { agent, codex_mode })
}

/// Ask what an interactive addition starts from when no option says. Branch
/// choices fill `creation`; a chosen issue's number is returned.
async fn pick_add_source(
    ctx: &Context,
    target: &AddTarget,
    creation: &mut Creation,
) -> Result<Option<String>> {
    if creation.branch.is_some()
        || creation.existing.is_some()
        || creation.base.is_some()
        || !ctx.interactive()
    {
        return Ok(None);
    }
    #[derive(Clone, Copy)]
    enum Source {
        New,
        Existing,
        Issue,
        Pull,
    }
    let source = ui::pick_choice(
        ctx,
        "Workspace> ",
        &[
            (Source::New, "Create a new branch"),
            (Source::Existing, "Use an existing branch"),
            (Source::Issue, "Use an issue"),
            (Source::Pull, "Use a pull request"),
        ],
    )?;
    match source {
        Source::New => {}
        Source::Existing => creation.existing = Some(pick_add_branch(ctx, target).await?),
        Source::Issue => {
            let repo = target.repository(ctx).await?;
            let issues = super::issues::origin_forge(repo)
                .await?
                .open_issues(&repo.path)
                .await?;
            return ui::pick_item(ctx, "Issue> ", "issues", issues).map(Some);
        }
        Source::Pull => {
            let repo = target.repository(ctx).await?;
            let forge = super::issues::origin_forge(repo).await?;
            let pulls = forge.open_pulls(&repo.path).await?;
            let number = ui::pick_item(ctx, "Pull request> ", "pull requests", pulls)?;
            let pull = forge.pull_request(&repo.path, &number).await?;
            super::links::pull_creation(ctx, repo, pull, creation).await?;
        }
    }
    Ok(None)
}

async fn select_add_creation(
    ctx: &Context,
    target: &AddTarget,
    mut creation: Creation,
    issue: Option<&super::issues::Issue>,
) -> Result<Creation> {
    if creation.branch.is_some() || creation.existing.is_some() {
        return Ok(creation);
    }
    if let Some(issue) = issue {
        return issue_creation(ctx, target, creation, issue.branch_name(), &issue.url).await;
    }
    // Check here so a typo can be corrected before creating work.
    creation.branch = Some(loop {
        let name = ui::input(ctx, "Branch name")?;
        match git::check_branch_name(None, &name).await {
            Ok(()) => break name,
            Err(error) => eprintln!("{error:#}"),
        }
    });
    Ok(creation)
}

/// Reopen the local branch or workspace that earlier work on the issue left
/// instead of suffixing a new branch, failing here when it cannot be reopened.
async fn issue_creation(
    ctx: &Context,
    target: &AddTarget,
    mut creation: Creation,
    name: String,
    issue_url: &str,
) -> Result<Creation> {
    let repo = target.repository(ctx).await?;
    let workspaces = client::workspaces(&ctx.paths).await?;
    let mut associated = None;
    for workspace in workspaces
        .iter()
        .filter(|workspace| workspace.repository_id == repo.id)
    {
        let inspection = client::inspect(&ctx.paths, workspace.id.clone()).await?;
        if inspection.issue.is_some_and(|issue| issue.url == issue_url) {
            ensure!(
                associated.is_none(),
                "multiple workspaces are associated with this issue; pass an explicit branch"
            );
            ensure!(
                workspace.state == WorkspaceState::Ready,
                "workspace {} is {}; inspect or set it up before reopening",
                workspace.name,
                workspace.state
            );
            associated = Some(workspace.branch.clone());
        }
    }
    if let Some(branch) = associated {
        creation.existing = Some(branch);
        return Ok(creation);
    }
    if let Some(workspace) = workspaces
        .into_iter()
        .find(|workspace| workspace.repository_id == repo.id && workspace.branch == name)
    {
        ensure!(
            workspace.state == WorkspaceState::Ready,
            "workspace {} is {}; inspect or set it up before reopening",
            workspace.name,
            workspace.state
        );
    } else if git::ref_exists(&repo.path, &git::local_ref(&name), git::isolated_command).await? {
        git::existing_branch::ensure_not_checked_out(&repo.path, &name).await?;
    } else {
        creation.branch = Some(name);
        return Ok(creation);
    }
    creation.existing = Some(name);
    Ok(creation)
}

async fn open_add_workspace(
    ctx: &Context,
    target: &AddTarget,
    creation: Creation,
) -> Result<OpenedWorkspace> {
    let Creation {
        path,
        branch,
        existing,
        base,
        git_profile,
    } = creation;
    let repository = target.selector.clone();
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
        let name = branch.context("a branch name is required")?;
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
    if ctx.herdr_tab.is_none() {
        shell::navigate(&workspace.path, ctx.json)?;
    }
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
            return match &ctx.herdr_tab {
                Some(tab) => tab.shell(&workspace.path).await,
                None => Ok(0),
            };
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

pub(super) async fn adopt(
    ctx: &Context,
    path: PathBuf,
    repository: Option<String>,
    copy: bool,
) -> Result<i32> {
    let path = super::repositories::absolute(ctx, path)?;
    let repository = match repository {
        Some(repository) => ui::repository_selector(repository)?,
        None => worktree_repository(ctx, &path).await?.into(),
    };
    let workspace = request::<Workspace>(
        &ctx.paths,
        Method::AdoptWorkspace {
            repository,
            path,
            copy,
        },
    )
    .await?;
    ctx.emit(
        &format!(
            "{} {} on branch {} at {} (normal cleanup applies)",
            if copy {
                "Copied and adopted"
            } else {
                "Adopted"
            },
            Palette::stdout(ctx.json).paint(Style::Heading, &workspace.name),
            workspace.branch,
            workspace.path.display()
        ),
        &workspace,
    )?;
    shell::navigate(&workspace.path, ctx.json)?;
    Ok(0)
}

/// The registered repository whose checkout owns the worktree at `path`,
/// otherwise the only one sharing its origin remote, otherwise one chosen
/// interactively.
async fn worktree_repository(ctx: &Context, path: &Path) -> Result<String> {
    let mut repos = client::repositories(&ctx.paths).await?;
    let common = git::common_dir(path)
        .await
        .with_context(|| format!("{} is not a Git worktree", path.display()))?;
    for repo in &repos {
        // Checkouts may keep their Git directory elsewhere; an unreadable one owns nothing.
        if git::common_dir(&repo.path)
            .await
            .is_ok_and(|dir| dir == common)
        {
            return Ok(repo.id.clone());
        }
    }
    if let Some(remote) = repository::identity(&path.to_string_lossy()).await? {
        let mut matches = Vec::new();
        for repo in &repos {
            // An unreadable registration cannot share the remote.
            if repository::identity(&repo.source)
                .await
                .ok()
                .flatten()
                .as_ref()
                == Some(&remote)
            {
                matches.push(repo.clone());
            }
        }
        match matches.as_slice() {
            [repo] => return Ok(repo.id.clone()),
            [] => {}
            _ => repos = matches,
        }
    }
    ensure!(
        ctx.interactive(),
        "cannot infer the repository of {}; pass --repo <repository>",
        path.display()
    );
    ui::pick(ctx, "Repository> ", ui::repository_choices(repos).await?)
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

/// Run the configured `post_setup_cmd`, if any, once the workspace is ready.
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
    let result = execution::setup(&ctx.paths, workspace.id.clone(), ctx.json).await;
    if let Err(error) = &result
        && let Some(failure) = error.downcast_ref::<execution::SetupVerificationFailure>()
    {
        return report_setup_verification_failure(ctx, workspace, failure.exit_code).await;
    }
    let failure = match result {
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

/// Process uncertainty cannot be ignored or safely stopped from the still-live
/// reporting wrapper. Keep the worktree and expose the daemon's actual evidence.
async fn report_setup_verification_failure(
    ctx: &Context,
    workspace: &Workspace,
    code: i32,
) -> Result<Option<Workspace>> {
    let inspection = client::inspect(&ctx.paths, workspace.id.clone()).await?;
    let name = &workspace.name;
    let reason = inspection
        .workspace
        .error
        .as_deref()
        .unwrap_or("process state is unknown");
    let message = format!(
        "Setup verification incomplete for {name} (command exit {code}): {reason}\nWorkspace retained at {}. Inspect with `shoal doctor {name}`; repair with `shoal doctor {name} --repair` after checking the reported processes. Use `--stop` to stop verified survivors.",
        workspace.path.display()
    );
    ensure!(ctx.interactive(), "{message}");
    eprintln!(
        "{}",
        Palette::stderr(ctx.json).paint(Style::Warning, message)
    );
    Ok(None)
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
    let stopped = ui::stopped_workspaces(&ctx.paths, &workspaces);
    ctx.show(&workspaces, |workspaces| {
        for row in ui::workspace_rows(workspaces, &[], &stopped, true, Palette::stdout(ctx.json)) {
            println!("{row}");
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

pub(super) async fn status(
    ctx: &Context,
    workspace: Option<String>,
    item: Option<String>,
) -> Result<i32> {
    let Some(target) = status_target(workspace.as_deref(), item)? else {
        let workspace = ui::select_workspace(ctx, workspace, Fallback::CurrentDirectory).await?;
        let status =
            request::<WorkspaceStatus>(&ctx.paths, Method::WorkspaceStatus { workspace }).await?;
        ctx.show(&status, |status| render_status(status, ctx.json))?;
        return Ok(0);
    };
    let workspaces =
        request::<Vec<Workspace>>(&ctx.paths, Method::FindWorkspaces { target }).await?;
    let mut statuses = Vec::new();
    for workspace in workspaces {
        let workspace = workspace.id;
        statuses.push(
            request::<WorkspaceStatus>(&ctx.paths, Method::WorkspaceStatus { workspace }).await?,
        );
    }
    ctx.show(&statuses, |statuses| {
        for (index, status) in statuses.iter().enumerate() {
            if index > 0 {
                println!();
            }
            render_status(status, ctx.json);
        }
    })?;
    Ok(0)
}

/// Items and resources find the workspaces that link or hold them; anything
/// else selects a workspace.
fn status_target(first: Option<&str>, item: Option<String>) -> Result<Option<WorkspaceTarget>> {
    match first {
        Some("resource") => Ok(Some(WorkspaceTarget::Resource {
            name: item.context("resource needs a pool or member name")?,
        })),
        Some(first)
            if matches!(first, "pr" | "issue") || IssueInput::parse(first) == IssueInput::Url =>
        {
            let selection = Selection::parse(Some(first.to_owned()), item)?;
            Ok(Some(WorkspaceTarget::Item {
                kind: selection.kind.context("expected an issue or PR")?,
                input: selection
                    .input
                    .with_context(|| format!("{first} needs a number or URL"))?,
            }))
        }
        _ => {
            ensure!(
                item.is_none(),
                "put pr, issue or resource before a number, URL or name"
            );
            Ok(None)
        }
    }
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
    super::base::render(workspace);
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
    println!("Holds:         {}", inspection.workspace.holds.len());
    for hold in &inspection.workspace.holds {
        println!(
            "  {}{}",
            hold.name,
            hold.reason
                .as_deref()
                .map_or(String::new(), |r| format!(" ({r})"))
        );
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
            "  {}/{} -> {} [{}]{}",
            resource.pool,
            resource.name,
            resource.resource,
            resource.mode,
            super::resources::repository_path(resource)
        );
    }

    if let Some(issue) = &inspection.issue {
        println!("Issue:         {}", issue.url);
        if let Some(error) = &issue.error {
            println!("  {}", palette.paint(Style::Warning, error));
        }
    }
    if let Some(completion) = &inspection.completion {
        println!(
            "Completion:    done ({})",
            if completion.cleanup {
                "cleanup requested"
            } else {
                "kept for review"
            }
        );
        if let Some(error) = &completion.error {
            println!("  {}", palette.paint(Style::Warning, error));
        }
    }
    match &inspection.pr_cleanup {
        Some(registration) => {
            match &registration.kind {
                RegistrationKind::Watch { urls, .. } => {
                    for url in urls {
                        println!("PR watch:      {url}");
                        if let Some(pr) = status.prs.iter().find(|pr| &pr.url == url) {
                            render_pr(pr, palette);
                        }
                    }
                }
                RegistrationKind::Acknowledgement { head } => println!("PR watch:      {head}"),
            }
            if let Some(error) = &registration.error {
                println!("  {}", palette.paint(Style::Warning, error));
            }
        }
        None => println!("PR watch:      none"),
    }
    if workspace.review.is_empty() {
        println!("Review:        none");
    } else {
        println!("Review:        ready for review");
        println!("{}", super::links::review_lines(&workspace.review));
    }
    println!("Notifications: {} unread", status.unread_notifications);
}

fn render_pr(pr: &PrStatus, palette: Palette) {
    if let Some(state) = pr.state {
        let conflicts = match pr.merge_conflicts {
            Some(true) => format!(", {}", palette.paint(Style::Error, "merge conflicts")),
            Some(false) => ", no merge conflicts".into(),
            None => String::new(),
        };
        println!("  State:       {state}{conflicts}");
    }
    if let Some(review) = pr.review {
        println!("  Review:      {}", palette.review_state(review));
    }
    if pr.state.is_some() {
        println!("  Checks:      {}", pr.checks.len());
    }
    for check in &pr.checks {
        println!(
            "    {}: {}",
            check.name,
            palette.check_result(&check.result)
        );
    }
    for error in &pr.errors {
        println!("  {}", palette.paint(Style::Warning, error));
    }
}

pub(super) async fn inspect(ctx: &Context, workspace: Option<String>) -> Result<i32> {
    let workspace = ui::select_workspace(ctx, workspace, Fallback::CurrentDirectory).await?;
    let inspection = client::inspect(&ctx.paths, workspace).await?;
    ctx.show(&inspection, |inspection| {
        println!(
            "{}",
            serde_json::to_string_pretty(inspection).unwrap_or_default()
        );
    })?;
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
            include_changes: ctx.interactive() && !yes,
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
    if !yes
        && (check.needs_choice()
            || keep_branch
            || delete_branch
            || !check.workspace.holds.is_empty())
    {
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
    if let Some(destination) = escape
        && (result.is_ok() || !std::env::current_dir().is_ok_and(|cwd| cwd.exists()))
    {
        shell::navigate(&destination, ctx.json)?;
    }
    let result = result?;
    if let Some(error) = &result.hook_error {
        eprintln!("warning: {error}");
    }
    let mut output = serde_json::to_value(&result)?;
    output["holds"] = serde_json::to_value(&check.workspace.holds)?;
    ctx.emit_styled(Style::Success, &removal_message(&result), &output)?;
    Ok(0)
}

fn confirm_removal(ctx: &Context, check: &RemovalCheck, choice: BranchChoice) -> Result<()> {
    let branch_action = match choice {
        BranchChoice::KeepBranch => "keep",
        BranchChoice::DeleteBranch => "delete (including unpushed commits)",
        BranchChoice::Auto => "delete if redundant; retain the default branch",
    };
    let mut action = format!(
        "Remove workspace: {}\nFiles:  delete, including uncommitted changes\nBranch: {} — {branch_action}",
        check.workspace.name,
        check.branch.as_deref().unwrap_or("none")
    );
    if !check.workspace.holds.is_empty() {
        action.push_str("\nHolds:");
        for hold in &check.workspace.holds {
            action.push_str("\n  ");
            action.push_str(&hold.name);
            if let Some(reason) = &hold.reason {
                action.push_str(&format!(" ({reason})"));
            }
        }
    }
    if ctx.interactive() && (!check.changed_files.is_empty() || check.changed_files_omitted > 0) {
        action.push_str("\nUncommitted changes and untracked files (Git status):");
        for file in &check.changed_files {
            action.push_str("\n  ");
            action.push_str(file);
        }
        if check.changed_files_omitted > 0 {
            action.push_str(&format!(
                "\n  ... {} more entries omitted; run git status in the workspace for the full list",
                check.changed_files_omitted
            ));
        }
    }
    ensure!(
        ui::confirm(ctx, &action, "--yes")?,
        "workspace removal canceled"
    );
    Ok(())
}

/// Where the shell should go if the current directory is inside `workspace`:
/// its repository's Shoal directory, or home when that is unavailable.
async fn escape_destination(ctx: &Context, workspace: &Workspace) -> Result<Option<PathBuf>> {
    let Some(cwd) = ui::current_directory()? else {
        return Ok(None);
    };
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
    execution::run_command(&ctx.paths, workspace, command).await
}

pub(super) async fn undone(ctx: &Context, workspace: Option<String>) -> Result<i32> {
    let workspace = ui::select_workspace(ctx, workspace, Fallback::CurrentDirectory).await?;
    let withdrawn: Option<Completion> =
        request(&ctx.paths, Method::WorkspaceUndone { workspace }).await?;
    let message = if withdrawn.is_some() {
        "Completion withdrawn; run shoal done when finished."
    } else {
        "No completion recorded."
    };
    ctx.emit(message, json!({ "withdrawn": withdrawn }))?;
    Ok(0)
}

pub(super) async fn done(
    ctx: &Context,
    workspace: Option<String>,
    cleanup: Option<bool>,
) -> Result<i32> {
    let workspace = ui::select_workspace(ctx, workspace, Fallback::CurrentDirectory).await?;
    // Calculate navigation before the daemon can remove the worktree. A failed
    // preview must not prevent recording completion, especially a keep default.
    let escape = if cleanup != Some(false) && !env::is_scoped() && !ctx.json {
        done_destination(ctx, &workspace).await.unwrap_or(None)
    } else {
        None
    };
    let completion: Completion =
        request(&ctx.paths, Method::WorkspaceDone { workspace, cleanup }).await?;
    if completion.cleanup
        && let Some(destination) = escape
    {
        shell::navigate(&destination, ctx.json)?;
    }
    ctx.emit(if completion.cleanup {
        "Assignment marked done; cleanup requested. Tracked commands may stop and the workspace may be removed."
    } else {
        "Assignment marked done; workspace kept for review."
    }, &completion)?;
    Ok(0)
}

async fn done_destination(ctx: &Context, workspace: &str) -> Result<Option<PathBuf>> {
    let check: RemovalCheck = request(
        &ctx.paths,
        Method::CheckRemoval {
            workspace: workspace.to_owned(),
            caller_pid: std::process::id(),
            include_changes: false,
        },
    )
    .await?;
    if check.dirty || check.unpushed_commits != 0 {
        return Ok(None);
    }
    escape_destination(ctx, &check.workspace).await
}

pub(super) async fn watch_items(
    ctx: &Context,
    workspace: String,
    selection: crate::forge::link::Selection,
    timeout: u64,
) -> Result<i32> {
    let (_stream, body) = client::open(
        &ctx.paths,
        Method::WatchItems {
            workspace: workspace.clone(),
            selection,
            timeout_secs: timeout,
        },
    )
    .await?;
    let updates = crate::forge::pr::wait::Updates::try_from(body)?;
    ctx.show(&updates, |updates| {
        for update in &updates.updates {
            println!("{} [{}]: {}", update.url, update.kind, update.message);
        }
        if updates.timed_out {
            println!("No item updates before timeout.");
        }
    })?;
    std::io::Write::flush(&mut std::io::stdout())?;
    if updates.updates.is_empty() {
        return Ok(0);
    }
    request::<()>(
        &ctx.paths,
        Method::AcknowledgePrUpdates {
            workspace,
            deliveries: updates
                .updates
                .iter()
                .map(|update| update.delivery.clone())
                .collect(),
        },
    )
    .await?;
    Ok(0)
}
