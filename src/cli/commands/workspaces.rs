//! CLI workspace workflows; all state mutations go through the daemon.
use std::{
    collections::BTreeMap,
    ffi::OsString,
    path::{Path, PathBuf},
    time::Duration,
};

use anyhow::{Context as _, Result, ensure};
use serde_json::json;
use tokio::time::Instant;

use crate::{
    agent::Agent,
    cli::{
        agents,
        client::{self, request},
        context::Context,
        output::{self, Palette, Style},
        recent, terminal,
        ui::{self, Fallback},
    },
    daemon::{
        approvals::Undecided,
        links::pr::RegistrationKind,
        recovery::{ReconcileOptions, Report},
    },
    env, execution,
    forge::{IssueInput, items::link::Selection, review::status::PrStatus},
    git::{self, existing_branch::Branch},
    hooks::{self, Hook, HookKind},
    model::{
        Chat, Completion, ConflictCheck, Dependency, DiffBase, OpenedWorkspace, Repository,
        Workspace, WorkspaceStatus, WorkspaceTarget,
    },
    protocol::{Body, ConfigTarget, Method},
    removal::{RemovalCheck, RemovalResult, UnsavedWork},
    repository, shell,
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
        let values = crate::env::export_configured(values, std::env::var_os("PATH"))?;
        ctx.show(&values, |values| {
            for (name, value) in values {
                println!("{name}={value}");
            }
        })?;
    }
    Ok(0)
}

/// `push` overrides `[land] push` when given.
pub(super) async fn land(
    ctx: &Context,
    workspace: Option<String>,
    push: Option<bool>,
) -> Result<i32> {
    let workspace = ui::select_workspace(ctx, workspace, Fallback::CurrentDirectory).await?;
    let push = match push {
        Some(push) => push,
        None => {
            client::settings(&ctx.paths, ConfigTarget::Workspace(workspace.clone()))
                .await?
                .land
                .push
        }
    };
    match execution::land(&ctx.paths, workspace, ctx.json, push).await {
        Err(error) => match error.downcast::<Undecided>() {
            Ok(Undecided(request)) => super::approvals::declined(ctx, &request),
            Err(error) => Err(error),
        },
        landed => landed,
    }
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

pub(super) async fn land_worker(ctx: &Context, plan: String, push: bool) -> Result<i32> {
    anyhow::ensure!(env::is_scoped(), "land worker requires a tracked execution");
    request::<()>(&ctx.paths, Method::CheckLanding).await?;
    let plan: crate::model::LandPlan = serde_json::from_str(&plan)?;
    anyhow::ensure!(
        std::env::var(env::WORKSPACE_ID)? == plan.workspace.id,
        "land worker requires its authorized workspace"
    );
    let result = crate::git::repo::finish_land(plan, push).await?;
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
        if let Some(remote) = &result.pushed_to {
            println!("Pushed {default} to {remote}");
        }
    })?;
    Ok(0)
}

/// Copy the main checkout's ignored files inside tracked setup, then become
/// the setup command so its exit status and signals are the execution's.
pub(super) async fn copy_ignored_worker(ctx: &Context, setup_cmd: Option<PathBuf>) -> Result<i32> {
    ensure!(
        env::is_scoped(),
        "copy-ignored worker requires a tracked execution"
    );
    // Tracked setup runs in the workspace root.
    let workspace = std::env::current_dir()?;
    git::worktrunk::copy_ignored(&ctx.paths.worktrunk_config(), &workspace).await?;
    match setup_cmd {
        Some(setup_cmd) => Err(become_setup(&setup_cmd)),
        None => Ok(0),
    }
}

/// Replace this process with `setup_cmd`; returns only the error when that fails.
fn become_setup(setup_cmd: &Path) -> anyhow::Error {
    let error =
        std::os::unix::process::CommandExt::exec(&mut std::process::Command::new(setup_cmd));
    anyhow::Error::new(error).context(format!("run {}", setup_cmd.display()))
}

pub(super) async fn diff(ctx: &Context, workspace: Option<String>) -> Result<i32> {
    let workspace = ui::select_workspace(ctx, workspace, Fallback::CurrentDirectory).await?;
    let base = request::<DiffBase>(&ctx.paths, Method::DiffBase { workspace }).await?;
    execution::run(
        &ctx.paths,
        base.workspace_id,
        git::diff_command(&base.commit),
        None,
    )
    .await
}

pub(super) async fn conflicts(
    ctx: &Context,
    workspace: Option<String>,
    target: Option<String>,
) -> Result<i32> {
    let workspace = ui::select_workspace(ctx, workspace, Fallback::CurrentDirectory).await?;
    let check =
        request::<ConflictCheck>(&ctx.paths, Method::Conflicts { workspace, target }).await?;
    ctx.show(&check, |check| {
        if check.conflicts {
            println!("Conflicts with {}:", check.target);
            for file in &check.files {
                println!("  {file}");
            }
        } else {
            println!("No conflicts with {}", check.target);
        }
    })?;
    Ok(i32::from(check.conflicts))
}

pub(super) async fn cd(ctx: &Context, workspace: Option<String>) -> Result<i32> {
    let workspace = match workspace {
        Some(workspace) => ui::explicit_workspace(ctx, workspace).await?,
        None => ui::workspace_picker(ctx).await?,
    };
    let inspection = client::inspect(&ctx.paths, workspace).await?;
    ensure!(
        inspection.workspace.path.is_dir(),
        "workspace directory is missing"
    );
    recent::record_move(ctx, &inspection.workspace.id).await;
    navigate(ctx, &inspection.workspace.path)?;
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
    /// The PR whose head branch is opened; it is linked once the workspace is ready.
    #[serde(default)]
    pub pr: Option<String>,
}

impl Creation {
    /// Build a new branch on `base`, the branch of a done dependency, unless
    /// the addition names its own base or opens an existing branch.
    pub(super) fn build_on(&mut self, base: Option<String>) {
        if self.base.is_none() && self.existing.is_none() {
            self.base = base;
        }
    }
}

/// How `shoal add` runs once its workspace is planned.
#[derive(Debug, Clone, Default)]
pub(super) struct AddOptions {
    /// Run in the current pane instead of opening a Herdr tab.
    pub here: bool,
    /// Focus the new tab or session whatever the `focus` setting says.
    pub focus: bool,
    /// Start the agent even when `max_agents` is reached.
    pub now: bool,
    /// Workspaces, queued agents and issues the agent waits for.
    pub after: Vec<String>,
}

impl AddOptions {
    /// Whether the agent skips the queue: a focused launch is one the user
    /// waits for, so it starts at once as `--now` does.
    fn starts_now(&self) -> bool {
        self.now || self.focus
    }
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
    options: AddOptions,
) -> Result<i32> {
    let mut creation = creation;
    creation.path = creation
        .path
        .map(|path| super::repositories::absolute(ctx, path))
        .transpose()?;
    let target = resolve_add_target(ctx, repository, issue.as_deref(), &agent).await?;
    check_add_creation(ctx, &target, &creation).await?;
    let after = after_dependencies(ctx, &target, &options.after).await?;
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
        Some(input) => Some(super::issues::load(ctx, target.repository(ctx).await?, &input).await?),
        None => None,
    };
    let creation = select_add_creation(ctx, &target, creation, issue.as_ref()).await?;
    // An issue or a typed name settles the branch; check it before the agent picker.
    check_add_creation(ctx, &target, &creation).await?;
    let agent = resolve_add_agent(ctx, &settings, agent).await?;
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
        queue_entry: None,
        chat: env::chat(),
        workspace: None,
    };
    ensure!(
        options.after.is_empty() || plan.agent_name().is_some(),
        "--after queues an agent; name one with --agent"
    );
    if let Some(agent) = plan.agent_name() {
        let title = issue.as_ref().map(|issue| issue.title.clone());
        let name = planned_workspace_name(ctx, &target, &plan.creation).await?;
        let launch = super::agent_queue::Launch {
            agent,
            name,
            issue_title: title,
            now: options.starts_now(),
            after: launch_dependencies(ctx, &target, issue.as_ref(), &options, after).await?,
        };
        match super::agent_queue::admit(ctx, &plan, launch).await? {
            super::agent_queue::Admission::Start { entry, base } => {
                plan.queue_entry = entry;
                plan.creation.build_on(base);
            }
            super::agent_queue::Admission::Queued(entry, waiting) => {
                return super::agent_queue::report_queued(ctx, &entry, waiting);
            }
        }
    }
    let handoff = start_beside(ctx, &target, &mut plan, issue.as_ref(), options).await;
    if let Some(entry) = &plan.queue_entry
        && let Err(error) = &handoff
    {
        super::agent_queue::fail(ctx, entry, error).await;
    }
    if handoff? {
        return Ok(0);
    }
    execute_add_with_target(ctx, plan, target, issue).await
}

/// The work `--after` names, refused before any prompt when the repository
/// has no such workspace, queued agent or issue.
async fn after_dependencies(
    ctx: &Context,
    target: &AddTarget,
    after: &[String],
) -> Result<Vec<Dependency>> {
    if after.is_empty() {
        return Ok(Vec::new());
    }
    let repo = target.repository(ctx).await?;
    let mut dependencies = Vec::new();
    for input in after {
        dependencies.push(match IssueInput::parse(input) {
            IssueInput::Number | IssueInput::Url => {
                Dependency::Issue(super::issues::url(ctx, repo, input).await?)
            }
            IssueInput::Invalid => Dependency::Work(input.clone()),
        });
    }
    request::<()>(
        &ctx.paths,
        Method::CheckDependencies {
            repository: target.selector.clone(),
            after: dependencies.clone(),
        },
    )
    .await?;
    Ok(dependencies)
}

/// The work a launch waits for: `after`, and the open issues the forge
/// records as blocking its issue. A launch that starts now waits for nothing.
async fn launch_dependencies(
    ctx: &Context,
    target: &AddTarget,
    issue: Option<&super::issues::Issue>,
    options: &AddOptions,
    mut after: Vec<Dependency>,
) -> Result<Vec<Dependency>> {
    if options.starts_now() {
        return Ok(Vec::new());
    }
    if let Some(issue) = issue {
        after.extend(issue_blockers(ctx, target, issue).await?);
    }
    Ok(after)
}

/// The open issues the forge records as blocking `issue`. A forge that
/// cannot list them, such as a repository with dependencies turned off,
/// leaves a warning and none.
async fn issue_blockers(
    ctx: &Context,
    target: &AddTarget,
    issue: &super::issues::Issue,
) -> Result<Vec<Dependency>> {
    let repo = target.repository(ctx).await?;
    match super::issues::open_dependencies(ctx, repo, issue).await {
        Ok(urls) => Ok(urls.into_iter().map(Dependency::Issue).collect()),
        Err(error) => {
            eprintln!(
                "warning: cannot list the issues blocking #{}, so the agent does not wait for them: {error:#}",
                issue.number
            );
            Ok(Vec::new())
        }
    }
}

/// Start `plan` beside this terminal, focused or in the background, unless
/// `options.here` keeps it in place. Returns whether it left this process.
async fn start_beside(
    ctx: &Context,
    target: &AddTarget,
    plan: &mut AddPlan,
    issue: Option<&super::issues::Issue>,
    options: AddOptions,
) -> Result<bool> {
    if options.here || !ctx.interactive() {
        return Ok(false);
    }
    let mut settings = client::settings(
        &ctx.paths,
        ConfigTarget::Repository(plan.repository.clone()),
    )
    .await?;
    if options.focus {
        settings.focus = Some(true);
    }
    let name = planned_workspace_name(ctx, target, &plan.creation).await?;
    terminal::start_add(ctx, &settings, plan, issue, &name).await
}

/// The workspace name `creation` will derive. An existing branch opens under
/// its local name, so a remote's prefix is left out, as opening it does.
async fn planned_workspace_name(
    ctx: &Context,
    target: &AddTarget,
    creation: &Creation,
) -> Result<String> {
    let branch = match (&creation.branch, &creation.existing) {
        (Some(branch), _) => branch.clone(),
        (None, Some(selector)) => match git::strip_local(selector) {
            Some(local) => local.to_owned(),
            None => {
                let repo = target.repository(ctx).await?;
                git::remote_branch(&repo.path, selector)
                    .await?
                    .map_or_else(|| selector.clone(), |(_, branch)| branch)
            }
        },
        (None, None) => String::new(),
    };
    Ok(crate::daemon::workspace::derive_workspace_name(&branch))
}

#[derive(serde::Serialize, serde::Deserialize)]
pub(in crate::cli) struct AddPlan {
    pub repository: crate::repository::Selector,
    pub creation: Creation,
    tab_label: Option<String>,
    #[serde(default)]
    pub tab_name: Option<crate::cli::terminal::PlaceName>,
    /// The found issue's URL; its body can exceed what a Herdr tab's
    /// environment carries, so the worker looks it up again.
    issue: Option<String>,
    agent: ResolvedAddAgent,
    args: Vec<OsString>,
    /// The agent queue entry whose slot this launch uses.
    #[serde(default)]
    pub queue_entry: Option<String>,
    /// The chat that planned the addition, which the workspace belongs to.
    #[serde(default)]
    chat: Option<String>,
    /// The existing workspace a launch in it reopens; it never creates one.
    #[serde(default)]
    workspace: Option<String>,
}

impl AddPlan {
    /// A launch of `agent` in the existing `workspace`, prompted with the
    /// issue at `issue` or with `args`. It runs detached, so it stays tracked.
    pub(super) fn agent_in(
        workspace: &Workspace,
        agent: Agent,
        issue: Option<&super::issues::Issue>,
        args: Vec<OsString>,
    ) -> Self {
        Self {
            repository: workspace.repository_id.as_str().into(),
            creation: Creation {
                path: None,
                branch: None,
                existing: Some(workspace.branch.clone()),
                base: None,
                git_profile: None,
                pr: None,
            },
            tab_label: None,
            tab_name: None,
            issue: issue.map(|issue| issue.url.clone()),
            agent: ResolvedAddAgent {
                agent: Some(agent),
                tracked: true,
            },
            args,
            queue_entry: None,
            chat: None,
            workspace: Some(workspace.id.clone()),
        }
    }

    /// Whether the plan opens `workspace`'s branch in its repository without
    /// creating anything.
    pub(super) fn opens(&self, workspace: &Workspace) -> bool {
        self.workspace.as_deref() == Some(workspace.id.as_str())
            && self.repository.value() == workspace.repository_id
            && self.creation.branch.is_none()
            && self.creation.path.is_none()
            && self.creation.existing.as_deref() == Some(workspace.branch.as_str())
    }

    /// The agent's name, when the addition starts one.
    pub fn agent_name(&self) -> Option<String> {
        self.agent.agent.clone().map(String::from)
    }

    /// The found issue's URL.
    pub fn issue_url(&self) -> Option<&str> {
        self.issue.as_deref()
    }

    /// Whether the agent starts with input: an issue prompt or forwarded arguments.
    /// A workspace shell or an agent without input waits for the user.
    pub fn runs_unattended(&self) -> bool {
        self.agent.agent.is_some() && (self.issue.is_some() || !self.args.is_empty())
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

/// Start an admitted queue entry's plan in the background where `shoal add`
/// would have from the terminal that queued it.
pub(in crate::cli) async fn start_queued(
    ctx: &Context,
    mut plan: AddPlan,
    name: &str,
) -> Result<()> {
    let settings = client::settings(
        &ctx.paths,
        ConfigTarget::Repository(plan.repository.clone()),
    )
    .await?;
    terminal::start_queued(ctx, &settings, &mut plan, name).await
}

pub(in crate::cli) async fn execute_add(ctx: &Context, plan: AddPlan) -> Result<i32> {
    let target = AddTarget {
        selector: plan.repository.clone(),
        repositories: tokio::sync::OnceCell::new(),
    };
    let issue = match &plan.issue {
        Some(url) => async { super::issues::load(ctx, target.repository(ctx).await?, url).await }
            .await
            .map(Some),
        None => Ok(None),
    };
    // A queued start reports a failed lookup as it reports any other failure.
    let issue = match (issue, &plan.queue_entry) {
        (Err(error), Some(entry)) => {
            super::agent_queue::fail(ctx, entry, &error).await;
            return Err(error);
        }
        (issue, _) => issue?,
    };
    execute_add_with_target(ctx, plan, target, issue).await
}

async fn execute_add_with_target(
    ctx: &Context,
    plan: AddPlan,
    target: AddTarget,
    issue: Option<super::issues::Issue>,
) -> Result<i32> {
    let Some(entry) = plan.queue_entry.clone() else {
        return add_planned(ctx, plan, target, issue).await;
    };
    let _hold = super::agent_queue::Hold::start(&ctx.paths, &entry).await;
    let result = add_planned(ctx, plan, target, issue).await;
    if let Err(error) = &result {
        super::agent_queue::fail(ctx, &entry, error).await;
    }
    result
}

async fn add_planned(
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
        queue_entry,
        chat,
        workspace,
    } = plan;
    let pr = creation.pr.clone();
    let opened = match workspace {
        Some(workspace) => {
            request::<OpenedWorkspace>(&ctx.paths, Method::ReopenWorkspace { workspace }).await?
        }
        None => open_add_workspace(ctx, &target, creation, chat).await?,
    };
    if let Some(entry) = &queue_entry {
        super::agent_queue::attach(ctx, entry, &opened.workspace).await?;
    }
    let label = match tab_name {
        Some(name) => name.render(&opened.workspace.branch),
        None => match &issue {
            Some(issue) => issue.tab_label(target.repository(ctx).await?),
            None => opened.workspace.branch.clone(),
        },
    };
    let agent_name = agent.agent.clone().map(String::from);
    terminal::host_workspace(ctx, &opened.workspace, agent_name.as_deref(), &label).await?;
    if let Some(issue) = &issue {
        request::<()>(
            &ctx.paths,
            Method::SetIssue {
                workspace: opened.workspace.id.clone(),
                url: issue.url.clone(),
                title: Some(issue.title.clone()),
            },
        )
        .await?;
    }
    let name = opened.workspace.name.clone();
    let Some(workspace) = finish_add_workspace(ctx, opened, pr, &agent, issue.is_some()).await?
    else {
        if let Some(entry) = &queue_entry {
            let error = anyhow::anyhow!(
                "setup of {name} did not complete; inspect it with `shoal doctor {name}`"
            );
            super::agent_queue::fail(ctx, entry, &error).await;
        }
        return Ok(1);
    };
    if let Some(entry) = &queue_entry {
        crate::env::launch_queue_entry(entry);
    }
    agent.launch(ctx, workspace, issue, args).await
}

/// Whether the caller's shell enters the added `workspace`, where `agent`
/// starts unless `None`. An agent that runs in a session this terminal
/// attaches to, given an issue prompt when `prompted`, leaves it where it
/// was, as detaching returns to that shell rather than to the workspace.
async fn enters_added(
    ctx: &Context,
    workspace: &Workspace,
    agent: Option<&ResolvedAddAgent>,
    prompted: bool,
) -> Result<bool> {
    if !terminal::follows_caller(ctx) {
        return Ok(false);
    }
    let Some((agent, chosen)) = agent.and_then(|agent| Some((agent, agent.agent.as_ref()?))) else {
        return Ok(true);
    };
    let settings =
        client::settings(&ctx.paths, ConfigTarget::Workspace(workspace.id.clone())).await?;
    Ok(!agents::runs_in_session(
        ctx,
        chosen,
        &settings,
        prompted,
        agent.tracked,
    ))
}

/// Link the PR a workspace was opened on and return the workspace with that
/// link. The workspace already exists, so a refused link only warns.
pub(super) async fn link_pull(
    ctx: &Context,
    workspace: Workspace,
    url: String,
) -> Result<Workspace> {
    let linked = request::<()>(
        &ctx.paths,
        Method::SetPr {
            workspace: workspace.id.clone(),
            action: crate::daemon::links::pr::Action::Watch { url: url.clone() },
        },
    )
    .await;
    if let Err(error) = linked {
        eprintln!("warning: cannot link PR {url}: {error:#}");
        return Ok(workspace);
    }
    Ok(client::inspect(&ctx.paths, workspace.id).await?.workspace)
}

struct AddTarget {
    selector: crate::repository::Selector,
    repositories: tokio::sync::OnceCell<Vec<Repository>>,
}

impl AddTarget {
    async fn repository(&self, ctx: &Context) -> Result<&Repository> {
        let repos = self
            .repositories
            .get_or_try_init(|| client::repositories(&ctx.paths))
            .await?;
        crate::repository::select(repos, &self.selector).await
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

/// Refuse what creation would refuse in the given repository, branch and
/// options before a prompt asks for the rest of the addition. Only a terminal
/// prompts; without one, creation refuses the arguments itself.
async fn check_add_creation(ctx: &Context, target: &AddTarget, creation: &Creation) -> Result<()> {
    if !ctx.interactive() {
        return Ok(());
    }
    request::<()>(
        &ctx.paths,
        Method::CheckCreation {
            repository: target.selector.clone(),
            branch: creation.branch.clone(),
            existing: creation.existing.clone(),
            base: creation.base.clone(),
            git_profile: creation.git_profile.clone(),
            path: creation.path.clone(),
        },
    )
    .await
}

#[derive(serde::Serialize, serde::Deserialize)]
#[serde(from = "StoredAddAgent")]
struct ResolvedAddAgent {
    agent: Option<Agent>,
    /// Whether the agent runs tracked, never handed to a desktop app.
    tracked: bool,
}

/// A plan's agent as queued entries store it, including plans queued before
/// `tracked`, which recorded Codex's mode instead.
#[derive(serde::Deserialize)]
struct StoredAddAgent {
    agent: Option<Agent>,
    #[serde(default)]
    tracked: bool,
    #[serde(default)]
    codex_mode: Option<crate::agent::CodexMode>,
}

/// Codex's CLI meant a tracked launch.
impl From<StoredAddAgent> for ResolvedAddAgent {
    fn from(stored: StoredAddAgent) -> Self {
        Self {
            agent: stored.agent,
            tracked: stored.tracked || stored.codex_mode == Some(crate::agent::CodexMode::Cli),
        }
    }
}

/// The repository's effective settings, loaded on first use.
struct AddSettings<'a> {
    ctx: &'a Context,
    repository: &'a crate::repository::Selector,
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
        agents::ensure_available(agent.name(), settings.get().await?)?;
    }
    Ok(AgentChoice::Chosen(agent))
}

async fn resolve_add_agent(
    ctx: &Context,
    settings: &AddSettings<'_>,
    agent: AgentChoice,
) -> Result<ResolvedAddAgent> {
    let agent = match agent {
        AgentChoice::Chosen(agent) => agent,
        AgentChoice::Picker => agents::pick_default_agent(ctx, settings.get().await?)?,
        AgentChoice::PickerIfInteractive if ctx.interactive() => {
            agents::pick_default_agent(ctx, settings.get().await?)?
        }
        AgentChoice::PickerIfInteractive => None,
    };
    Ok(ResolvedAddAgent {
        agent,
        tracked: false,
    })
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
            let issues = super::issues::origin_forge(ctx, repo)
                .await?
                .open_issues(&ctx.forge_account()?)
                .await?;
            return ui::pick_item(ctx, "Issue> ", "issues", issues).map(Some);
        }
        Source::Pull => {
            let repo = target.repository(ctx).await?;
            let forge = super::issues::origin_forge(ctx, repo).await?;
            let account = ctx.forge_account()?;
            let pulls = forge.open_pulls(&account).await?;
            let number = ui::pick_item(ctx, "Pull request> ", "pull requests", pulls)?;
            let pull = forge.pull_request(&account, &number).await?;
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
        if workspace.links.issue.as_deref() == Some(issue_url) {
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

/// Open the planned workspace for `chat`, or for the user without one.
async fn open_add_workspace(
    ctx: &Context,
    target: &AddTarget,
    creation: Creation,
    chat: Option<String>,
) -> Result<OpenedWorkspace> {
    let Creation {
        path,
        branch,
        existing,
        base,
        git_profile,
        pr: _,
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
                chat,
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
                chat,
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

/// Set up an opened workspace and link its PR, which needs a ready workspace,
/// before reporting it and entering it unless `agent`, given an issue prompt
/// when `prompted`, takes the terminal.
async fn finish_add_workspace(
    ctx: &Context,
    opened: OpenedWorkspace,
    pr: Option<String>,
    agent: &ResolvedAddAgent,
    prompted: bool,
) -> Result<Option<Workspace>> {
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
    if let Some(url) = pr {
        workspace = link_pull(ctx, workspace, url).await?;
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
    // The post-setup hook may change how the agent launches, so the shell's
    // destination waits for it; a failed hook starts no agent.
    let post_setup = if reused {
        Ok(())
    } else {
        run_post_setup(ctx, &workspace).await
    };
    let starts = post_setup.is_ok().then_some(agent);
    if enters_added(ctx, &workspace, starts, prompted).await? {
        shell::navigate(&workspace.path, ctx.json)?;
    }
    post_setup?;
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
            return terminal::open_shell(ctx, &workspace.path).await;
        };
        let prompt = issue.map(|issue| issue.prompt());
        let options = agents::LaunchOptions {
            tracked: self.tracked,
            ..Default::default()
        };
        agents::launch_agent(ctx, agent, workspace, prompt, args, options).await
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

/// The variables `[env]` adds to the workspace's tracked executions and hooks.
pub(in crate::cli) async fn configured_environment(
    ctx: &Context,
    workspace: &Workspace,
) -> Result<BTreeMap<String, OsString>> {
    let settings =
        client::settings(&ctx.paths, ConfigTarget::Workspace(workspace.id.clone())).await?;
    let repo = client::repositories(&ctx.paths)
        .await?
        .into_iter()
        .find(|repo| repo.id == workspace.repository_id)
        .context("workspace repository is missing")?;
    Ok(crate::env::render_configured_environment(
        &settings.env,
        workspace,
        repository::name(&repo),
        &client::inspect(&ctx.paths, workspace.id.clone())
            .await?
            .ports,
        // Only the daemon creates them; tools create a missing one.
        &crate::caches::locate(&repo, &settings.caches),
    ))
}

/// The workspace's [`configured_environment`] for finding its agents' files,
/// or none with a warning when it cannot be resolved.
pub(in crate::cli) async fn agent_environment(
    ctx: &Context,
    workspace: &Workspace,
) -> BTreeMap<String, OsString> {
    configured_environment(ctx, workspace)
        .await
        .unwrap_or_else(|error| {
            eprintln!(
                "warning: cannot resolve the [env] of {} for its agents: {error:#}",
                workspace.name
            );
            Default::default()
        })
}

/// Run the configured `post_setup_cmd`, if any, once the workspace is ready.
/// A failure keeps the ready workspace and stops what would follow.
pub(super) async fn run_post_setup(ctx: &Context, workspace: &Workspace) -> Result<()> {
    let command = request::<Option<PathBuf>>(
        &ctx.paths,
        Method::WorkspaceHook {
            workspace: workspace.id.clone(),
            kind: HookKind::PostSetup,
        },
    )
    .await?;
    if let Some(command) = command {
        let configured_env = configured_environment(ctx, workspace).await?;
        hooks::run_interactive(
            Hook::PostSetup,
            workspace,
            &command,
            &ctx.paths,
            ctx.json,
            &configured_env,
        )
        .await?;
    }
    Ok(())
}

/// Run the setup command, then let an interactive user decide what to do with
/// a failed workspace. `None` means the workspace was deleted or kept as-is.
pub(super) async fn setup_workspace(
    ctx: &Context,
    workspace: &Workspace,
) -> Result<Option<Workspace>> {
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
            "setup failed for {name}: {error}; workspace retained. Retry with `shoal setup {name}`, ignore with `shoal doctor {name} --repair`, or delete with `shoal rm {name} --yes`"
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
            "Delete workspace {} at {} (including setup changes)?",
            workspace.name,
            workspace.path.display(),
        ),
        "--yes",
    )?;
    if confirmed {
        request::<RemovalResult>(
            &ctx.paths,
            Method::RemoveWorkspace {
                workspace: workspace.id.clone(),
                unsaved: UnsavedWork::All,
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

pub(super) async fn list(
    ctx: &Context,
    queue: bool,
    filter: &super::listing::ListFilter,
) -> Result<i32> {
    if queue {
        return super::agent_queue::list(ctx, filter).await;
    }
    // Listing chats first ends finished ones, returning their workspaces to the user.
    let chats = super::chat::listed(ctx, filter).await?;
    let mut workspaces = client::workspaces(&ctx.paths).await?;
    let stopped = ui::stopped_workspaces(&ctx.paths, &workspaces);
    workspaces
        .retain(|workspace| filter.keeps_workspace(workspace, stopped.contains(&workspace.id)));
    let queued =
        super::agent_queue::rows(ctx, &super::agent_queue::listed(ctx, filter).await?).await?;
    let repositories = super::chat::repositories(ctx, &chats).await?;
    let rows = ui::workspace_tree_rows(
        &workspaces,
        &chats,
        &repositories,
        &stopped,
        Palette::stdout(ctx.json),
        output::stdout_width(ctx.json),
    );
    ctx.show(&workspaces, |_| {
        for row in rows.into_iter().chain(queued) {
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
        let status = StatusView::request(ctx, workspace).await?;
        ctx.show(&status, |status| render_status(status, ctx.json))?;
        return Ok(0);
    };
    let workspaces =
        request::<Vec<Workspace>>(&ctx.paths, Method::FindWorkspaces { target }).await?;
    let mut statuses = Vec::new();
    for workspace in workspaces {
        statuses.push(StatusView::request(ctx, workspace.id).await?);
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

/// The daemon's workspace status with the agent sessions the CLI finds.
#[derive(serde::Serialize)]
struct StatusView {
    #[serde(flatten)]
    status: WorkspaceStatus,
    sessions: Vec<terminal::Session>,
    /// Why sessions could not be listed; status still reports the rest.
    #[serde(skip_serializing_if = "Option::is_none")]
    sessions_error: Option<String>,
    /// How text names the workspace's chat; JSON gives its ID.
    #[serde(skip)]
    chat: Option<String>,
}

impl StatusView {
    async fn request(ctx: &Context, workspace: String) -> Result<Self> {
        // Listing chats first ends finished ones, returning their workspaces to the user.
        let chats = client::chats(&ctx.paths).await?;
        let status =
            request::<WorkspaceStatus>(&ctx.paths, Method::WorkspaceStatus { workspace }).await?;
        let chat = chat_name(ctx, &chats, &status.inspection.workspace).await?;
        let (sessions, sessions_error) =
            match terminal::sessions(&status.inspection.workspace.id).await {
                Ok(sessions) => (sessions, None),
                Err(error) => (Vec::new(), Some(format!("{error:#}"))),
            };
        Ok(Self {
            status,
            sessions,
            sessions_error,
            chat,
        })
    }
}

/// The listed chat that opened `workspace` with its agent, else its ID, which
/// scoped callers see without the chat.
async fn chat_name(ctx: &Context, chats: &[Chat], workspace: &Workspace) -> Result<Option<String>> {
    let Some(id) = &workspace.chat else {
        return Ok(None);
    };
    let Some(chat) = chats.iter().find(|chat| chat.id == *id) else {
        return Ok(Some(id.clone()));
    };
    let repositories = super::chat::repositories(ctx, std::slice::from_ref(chat)).await?;
    Ok(Some(format!(
        "{} ({})",
        ui::chat_label(chat, &repositories),
        chat.agent
    )))
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

fn render_status(view: &StatusView, json: bool) {
    let status = &view.status;
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
    super::split::render(workspace);
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

    println!("Sessions:      {}", view.sessions.len());
    for session in &view.sessions {
        println!("  {}", session.label());
    }
    if let Some(error) = &view.sessions_error {
        println!("  {}", palette.paint(Style::Warning, error));
    }

    println!(
        "Agent state:   {}",
        workspace
            .agent_state
            .map_or("none", |status| { ui::agent_state_label(status.state) })
    );
    println!("Chat:          {}", view.chat.as_deref().unwrap_or("none"));

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
        if let Some(state) = status.issue_status.as_ref().and_then(|s| s.state.as_ref()) {
            println!("  State:       {state}");
        }
        let lookup = status.issue_status.as_ref().and_then(|s| s.error.as_ref());
        for error in issue.error.iter().chain(lookup) {
            println!("  {}", palette.paint(Style::Warning, error));
        }
    }
    if let Some(completion) = &inspection.completion {
        let choice = if completion.cleanup {
            "cleanup requested"
        } else {
            "no cleanup requested"
        };
        println!("Completion:    done ({choice})");
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
    render_recorded("Opened:", &workspace.links.opened);
    render_recorded("Related:", &workspace.links.related);
    println!("Cleanup:       {}", status.cleanup_reason);
    if let Some(at) = status.cleanup_at {
        println!(
            "Grace period:  ends {}",
            crate::time::local_minutes(at as i64)
        );
    }
    if let Some(error) = &status.cleanup_error {
        println!("Auto cleanup:  {}", palette.paint(Style::Warning, error));
    }
    if workspace.review.is_empty() {
        println!("Review:        none");
    } else {
        println!("Review:        ready for review");
        println!("{}", super::links::review_lines(&workspace.review));
    }
    println!("Notifications: {} unread", status.unread_notifications);
}

fn render_recorded(label: &str, items: &[crate::model::LinkedItem]) {
    for item in items {
        match &item.title {
            Some(title) => println!("{label:<15}{} {title}", item.url),
            None => println!("{label:<15}{}", item.url),
        }
    }
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

pub(super) async fn remove(ctx: &Context, workspace: Option<String>, yes: bool) -> Result<i32> {
    if let Some(selector) = &workspace
        && let Some(code) = super::agent_queue::remove(ctx, selector).await?
    {
        return Ok(code);
    }
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
    let unsaved = check.unsaved_work();
    if !yes && (!unsaved.is_empty() || !check.workspace.holds.is_empty()) {
        confirm_removal(ctx, &check)?;
    }
    let result = remove_checked(ctx, workspace, &check).await?;
    let mut output = serde_json::to_value(&result)?;
    output["holds"] = serde_json::to_value(&check.workspace.holds)?;
    ctx.emit_styled(Style::Success, &removal_message(&result), &output)?;
    Ok(0)
}

/// Remove the workspace `check` inspected, deleting only the unsaved work it
/// saw: more appearing meanwhile makes the daemon refuse.
async fn remove_checked(
    ctx: &Context,
    workspace: String,
    check: &RemovalCheck,
) -> Result<RemovalResult> {
    let method = Method::RemoveWorkspace {
        workspace,
        unsaved: UnsavedWork::confirming(check),
        caller_pid: std::process::id(),
    };
    let result = leaving(
        ctx,
        &check.workspace,
        request::<RemovalResult>(&ctx.paths, method),
    )
    .await?;
    if let Some(error) = &result.hook_error {
        eprintln!("warning: {error}");
    }
    Ok(result)
}

/// Run `removal`, which removes `workspace`, after finding where the shell
/// goes when its directory disappears under it, and send it there.
pub(super) async fn leaving<T>(
    ctx: &Context,
    workspace: &Workspace,
    removal: impl std::future::Future<Output = Result<T>>,
) -> Result<T> {
    let escape = escape_destination(ctx, workspace).await?;
    let result = removal.await;
    if let Some(destination) = escape
        && (result.is_ok() || !std::env::current_dir().is_ok_and(|cwd| cwd.exists()))
    {
        shell::navigate(&destination, ctx.json)?;
    }
    result
}

fn confirm_removal(ctx: &Context, check: &RemovalCheck) -> Result<()> {
    let mut action = format!("Remove workspace: {}", check.workspace.name);
    for work in check.unsaved_work() {
        action.push_str(&format!("\nDeletes: {work}"));
    }
    if !check.unsaved_work().is_empty() {
        action.push_str(if !check.dirty || check.unsaved_files_archived {
            "\nshoal restore brings them back until the checkpoint expires."
        } else {
            "\nThe checkpoint cannot keep the uncommitted files (they are too large, or include a repository or a changed submodule), so they cannot be restored."
        });
    }
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
    match &result.branch {
        Some(branch) if !result.branch_outcome.is_deleted() => {
            format!("Workspace removed; kept Git branch {branch}")
        }
        _ => "Workspace removed".into(),
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

/// The configured command `shoal edit` runs instead of `$VISUAL` or `$EDITOR`.
const EDIT: &str = "edit";

pub(super) async fn edit(
    ctx: &Context,
    workspace: Option<String>,
    args: Vec<OsString>,
) -> Result<i32> {
    let workspace = ui::select_workspace(ctx, workspace, Fallback::CurrentDirectory).await?;
    let settings = client::settings(&ctx.paths, ConfigTarget::Workspace(workspace.clone())).await?;
    if settings.commands.contains_key(EDIT) {
        return super::configured::run(ctx, EDIT, Some(workspace), args).await;
    }
    let mut command = editor([std::env::var_os("VISUAL"), std::env::var_os("EDITOR")])?;
    command.push(
        client::inspect(&ctx.paths, workspace.clone())
            .await?
            .workspace
            .path
            .into(),
    );
    command.extend(args);
    execution::run_command(&ctx.paths, workspace, command).await
}

/// The first set of `$VISUAL` and `$EDITOR` as an argument array: split on
/// whitespace, as in `code --wait`, unless it is not UTF-8.
fn editor(values: [Option<OsString>; 2]) -> Result<Vec<OsString>> {
    let value = values
        .into_iter()
        .flatten()
        .find(|value| !value.to_string_lossy().trim().is_empty())
        .context("set $VISUAL, $EDITOR or [commands] edit to open a workspace")?;
    Ok(match value.to_str() {
        Some(value) => value.split_whitespace().map(OsString::from).collect(),
        None => vec![value],
    })
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

pub(super) async fn done(ctx: &Context, workspace: Option<String>, cleanup: bool) -> Result<i32> {
    let workspace = ui::select_workspace(ctx, workspace, Fallback::CurrentDirectory).await?;
    let completion: Completion =
        request(&ctx.paths, Method::WorkspaceDone { workspace, cleanup }).await?;
    let message = if completion.cleanup {
        "Assignment marked done; cleanup requested. Tracked commands may stop and the workspace may be removed."
    } else {
        "Assignment marked done; no cleanup requested."
    };
    ctx.emit(message, &completion)?;
    Ok(0)
}

pub(super) async fn watch_items(
    ctx: &Context,
    workspace: String,
    selection: crate::forge::items::link::Selection,
    timeout: Option<u64>,
    resumed: bool,
) -> Result<i32> {
    // The installed release continues the wait after an upgrade.
    let executable = crate::cli::installed::Executable::current();
    let deadline = timeout.map(|timeout| Instant::now() + Duration::from_secs(timeout));
    let body = wait_for_items(ctx, &workspace, selection, deadline, resumed)
        .await
        .map_err(|error| {
            executable.resume_or(error, || resume_watch_arguments(&workspace, deadline))
        })?;
    let updates = crate::daemon::links::wait::Updates::try_from(body)?;
    ctx.show(&updates, |updates| {
        for message in &updates.messages {
            println!("Message: {}", message.message);
        }
        for update in &updates.updates {
            println!("{update}");
        }
        if updates.timed_out {
            println!("No item updates before timeout.");
        }
        if updates.superseded {
            println!("A newer wait in this workspace receives the updates.");
        }
    })?;
    std::io::Write::flush(&mut std::io::stdout())?;
    if updates.updates.is_empty() && updates.messages.is_empty() {
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
            messages: updates.messages.iter().map(|message| message.id).collect(),
        },
    )
    .await?;
    Ok(0)
}

/// A daemon restart closes the wait, so wait again on the next daemon until
/// `deadline`; `restarted` says the daemon restarted before this wait began.
/// Waiting acknowledges nothing, and undelivered updates stay pending for the
/// next wait.
pub(super) async fn wait_for_items(
    ctx: &Context,
    workspace: &str,
    selection: Selection,
    deadline: Option<Instant>,
    restarted: bool,
) -> Result<Body> {
    let method = || Method::WatchItems {
        workspace: workspace.to_owned(),
        selection: selection.clone(),
        timeout_secs: seconds_left(deadline),
    };
    let reply = client::await_reply(&ctx.paths, method, restarted);
    let Some(deadline) = deadline else {
        return reply.await;
    };
    match tokio::time::timeout_at(deadline, reply).await {
        Ok(reply) => reply,
        Err(_) => Ok(Body::PrUpdates(crate::daemon::links::wait::Updates {
            updates: Vec::new(),
            messages: Vec::new(),
            timed_out: true,
            superseded: false,
        })),
    }
}

/// The whole seconds the daemon waits before `deadline`, at least one.
fn seconds_left(deadline: Option<Instant>) -> Option<u64> {
    let left = deadline?.saturating_duration_since(Instant::now());
    Some((left.as_secs() + u64::from(left.subsec_nanos() > 0)).max(1))
}

/// This watch's arguments continuing its wait in `workspace` until `deadline`,
/// as the installed release does after an upgrade restarted the daemon.
fn resume_watch_arguments(workspace: &str, deadline: Option<Instant>) -> Vec<OsString> {
    use crate::cli::installed::{arguments, replace_option};
    let mut resumed = replace_option(arguments(), "--workspace", workspace);
    if let Some(seconds) = seconds_left(deadline) {
        resumed = replace_option(resumed, "--timeout", &seconds.to_string());
    }
    if !resumed.iter().any(|argument| argument == "--resume") {
        resumed.push("--resume".into());
    }
    resumed
}

#[cfg(test)]
mod tests {
    use super::editor;
    use std::time::Duration;

    #[test]
    fn a_launch_in_a_workspace_opens_only_that_workspace() {
        use crate::{model::Workspace, state::WorkspaceState};
        let workspace = |repository: &str, branch: &str| {
            Workspace::new_record(
                repository.into(),
                branch.into(),
                format!("/work/{branch}").into(),
                branch.into(),
                WorkspaceState::Ready,
            )
        };
        let child = workspace("repo", "child");
        let plan = super::AddPlan::agent_in(&child, "claude".parse().unwrap(), None, Vec::new());
        assert!(plan.opens(&child));
        // The same branch in another repository, or another branch, is not the target.
        assert!(!plan.opens(&workspace("other", "child")));
        assert!(!plan.opens(&workspace("repo", "sibling")));
        let mut creating =
            super::AddPlan::agent_in(&child, "claude".parse().unwrap(), None, Vec::new());
        creating.creation.branch = Some("new".into());
        assert!(!creating.opens(&child));
    }

    #[test]
    fn plans_queued_with_codex_cli_stay_tracked() {
        let decode = |json: &str| serde_json::from_str::<super::ResolvedAddAgent>(json).unwrap();
        assert!(decode(r#"{"agent":"codex","codex_mode":"cli"}"#).tracked);
        assert!(!decode(r#"{"agent":"codex","codex_mode":"app"}"#).tracked);
        assert!(!decode(r#"{"agent":"claude","codex_mode":null}"#).tracked);
        assert!(decode(r#"{"agent":"codex","tracked":true}"#).tracked);
    }
    use std::ffi::OsString;

    #[test]
    fn editor_prefers_visual_splits_on_whitespace_and_requires_a_value() {
        assert_eq!(
            editor([Some(" code  --wait ".into()), Some("vi".into())]).unwrap(),
            [OsString::from("code"), OsString::from("--wait")]
        );
        for visual in [None, Some(" ".into())] {
            assert_eq!(editor([visual, Some("vi".into())]).unwrap(), ["vi"]);
        }
        for missing in [None, Some(OsString::new()), Some(" ".into())] {
            assert!(editor([missing.clone(), missing]).is_err());
        }
    }

    fn deadline(seconds: u64) -> tokio::time::Instant {
        tokio::time::Instant::now() + Duration::from_secs(seconds)
    }

    #[tokio::test]
    async fn item_wait_continues_on_the_replacement_daemon() {
        use crate::{
            cli::context::Context,
            daemon::links::wait::Updates,
            paths::Paths,
            protocol::{self, Body, ErrorCode, Method, Request, Response},
        };
        let root = tempfile::tempdir_in("/tmp").unwrap();
        let paths = Paths::for_test(root.path());
        paths.prepare().unwrap();
        let socket = paths.socket.clone();
        let listener = tokio::net::UnixListener::bind(&socket).unwrap();
        let server = tokio::spawn(async move {
            // The restarting daemon drops the wait without a reply.
            let (mut stream, _) = listener.accept().await.unwrap();
            let request: Request = protocol::read(&mut stream).await.unwrap();
            assert!(matches!(
                request.method,
                Method::WatchItems {
                    timeout_secs: Some(600),
                    ..
                }
            ));
            drop(stream);
            // Its socket goes with it, and the next daemon listens a moment later.
            drop(listener);
            std::fs::remove_file(&socket).unwrap();
            tokio::time::sleep(Duration::from_millis(500)).await;
            let listener = tokio::net::UnixListener::bind(&socket).unwrap();
            // It knows the agent's scope once the agent's wrapper reattaches.
            let (mut stream, _) = listener.accept().await.unwrap();
            let request: Request = protocol::read(&mut stream).await.unwrap();
            let refused = Body::error(ErrorCode::ScopeDenied, "expired or unknown workspace scope");
            protocol::write(&mut stream, &Response::new(request.id, refused))
                .await
                .unwrap();
            let (mut stream, _) = listener.accept().await.unwrap();
            let request: Request = protocol::read(&mut stream).await.unwrap();
            let Method::WatchItems {
                workspace,
                timeout_secs,
                ..
            } = request.method
            else {
                panic!("expected an item wait");
            };
            assert_eq!(workspace, "work");
            assert!((1..=600).contains(&timeout_secs.unwrap()));
            let updates = Updates {
                updates: Vec::new(),
                messages: Vec::new(),
                timed_out: true,
                superseded: false,
            };
            protocol::write(
                &mut stream,
                &Response::new(request.id, Body::PrUpdates(updates)),
            )
            .await
            .unwrap();
        });
        let ctx = Context::new(paths, true);
        let body = tokio::time::timeout(
            Duration::from_secs(60),
            super::wait_for_items(&ctx, "work", Default::default(), Some(deadline(600)), false),
        )
        .await
        .unwrap()
        .unwrap();
        assert!(Updates::try_from(body).unwrap().timed_out);
        server.await.unwrap();
    }

    #[tokio::test(start_paused = true)]
    async fn item_wait_ends_at_its_deadline_while_the_daemon_is_away() {
        use crate::{
            cli::context::Context,
            daemon::links::wait::Updates,
            paths::Paths,
            protocol::{self, Method, Request},
        };
        let root = tempfile::tempdir_in("/tmp").unwrap();
        let paths = Paths::for_test(root.path());
        paths.prepare().unwrap();
        let listener = tokio::net::UnixListener::bind(&paths.socket).unwrap();
        // The daemon drops the wait and then accepts nothing more.
        let server = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await?;
            let request: Request = protocol::read(&mut stream).await?;
            drop(stream);
            anyhow::Ok((request, listener))
        });
        let ctx = Context::new(paths, true);
        let started = tokio::time::Instant::now();
        // The paused clock reaches the deadline only once nothing else can run.
        let body =
            super::wait_for_items(&ctx, "work", Default::default(), Some(deadline(600)), false)
                .await
                .unwrap();
        assert!(Updates::try_from(body).unwrap().timed_out);
        assert!(started.elapsed() >= Duration::from_secs(600));
        let (request, _listener) = server.await.unwrap().unwrap();
        assert!(matches!(
            request.method,
            Method::WatchItems {
                timeout_secs: Some(600),
                ..
            }
        ));
    }
}
