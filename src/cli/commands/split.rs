//! `shoal split`: a child workspace on a new branch, which the parent
//! workspace's agent works on and receives the activity of, or which an agent
//! of its own works on, which may start from a fork of the parent agent's
//! session; or a workspace at a commit, with no branch or agent of its own.
use std::ffi::OsString;

use anyhow::{Context as _, Result};

use crate::client::output::split::Child;
use crate::{
    agent::{Agent, ModelChoice},
    cli::{
        agents,
        client::{self, request},
        context::Context,
        output::{Palette, Style},
        terminal,
        ui::{self, Fallback},
    },
    daemon::refusal::{Reason, Refusal},
    model::{AgentSession, QueueEntry, SpaceRef, Workspace},
    protocol::{
        AgentMethod, CommitStart, ConfigTarget, CreationOptions, OperationMethod, WorkspaceMethod,
        operations::StartedWorkspace,
    },
    shell,
    state::{QueueState, WorkspaceState},
};

use super::{issues::Issue, workspaces::AddPlan};

/// What `shoal split` asks for besides the parent.
pub(super) struct Split {
    pub branch: Option<String>,
    pub issue: Option<String>,
    pub base: Option<String>,
    pub agent: Option<Agent>,
    /// Whether the child's agent starts from a fork of the parent agent's
    /// session.
    pub fork: bool,
    /// The model and effort requested over the configured ones.
    pub model: ModelChoice,
    pub options: CreationOptions,
    pub args: Vec<OsString>,
}

/// The agent the child starts with the requested `model`, from a fork of
/// session `fork` when given.
struct ChildAgent {
    agent: Agent,
    model: ModelChoice,
    fork: Option<String>,
}

pub(super) async fn run(ctx: &Context, workspace: Option<String>, split: Split) -> Result<i32> {
    // A supplied branch, or a model for no agent, fails before the parent
    // picker opens.
    if let Some(branch) = &split.branch {
        crate::git::check_branch_name(None, branch).await?;
    }
    anyhow::ensure!(
        split.agent.is_some() || split.fork || split.model.is_empty(),
        "--model and --effort need --agent or --fork"
    );
    let parent = ui::select_workspace(ctx, workspace, Fallback::CurrentDirectory).await?;
    let agent = child_agent(ctx, &parent, split.agent, split.fork, split.model).await?;
    let issue = match &split.issue {
        Some(input) => Some(load_issue(ctx, &parent, input).await?),
        None => None,
    };
    let name = match (split.branch, &issue) {
        (Some(branch), _) => branch,
        (None, Some(issue)) => issue.branch_name(),
        (None, None) => anyhow::bail!("name the child's branch or pass --issue"),
    };
    let child = start_child(ctx, parent, name, split.base, issue.as_ref(), split.options).await?;
    let Some(child) = follow_preparation(ctx, child).await? else {
        return Ok(1);
    };
    let agent = match agent {
        Some(agent) => Some(start_agent(ctx, &child, agent, issue.as_ref(), split.args).await?),
        None => None,
    };
    report(ctx, &child, agent.as_ref())?;
    Ok(0)
}

/// Create a workspace at `base`, or at the default branch, without a branch:
/// a child of the named or current workspace, or outside one a top-level
/// workspace in the current repository.
pub(super) async fn at_commit(
    ctx: &Context,
    workspace: Option<String>,
    base: Option<String>,
    options: CreationOptions,
) -> Result<i32> {
    let (repository, parent) = commit_owner(ctx, workspace).await?;
    let method = WorkspaceMethod::CreateCommitWorkspace {
        repository,
        parent: parent.or_else(|| ui::current_chat(&None)),
        start: CommitStart::Revision(base),
        options,
    };
    let Some(child) = prepare(ctx, request(&ctx.paths, method).await?).await? else {
        return Ok(1);
    };
    report(ctx, &child, None)?;
    Ok(0)
}

/// The repository and parent of a workspace at a commit: the named or
/// current workspace's, or without one the current repository's and none.
async fn commit_owner(
    ctx: &Context,
    workspace: Option<String>,
) -> Result<(crate::repository::Selector, Option<String>)> {
    match ui::choose_workspace(ctx, workspace, Fallback::CurrentDirectoryOnly).await {
        Ok(parent) => {
            let repository = client::workspaces(&ctx.paths)
                .await?
                .into_iter()
                .find(|workspace| workspace.id == parent || workspace.name == parent)
                .map(|workspace| workspace.repository_id)
                .ok_or_else(|| {
                    Refusal::new(Reason::NotFound, format!("unknown workspace: {parent}"))
                })?;
            Ok((repository.into(), Some(parent)))
        }
        Err(error) if error.is::<ui::NoCurrentWorkspace>() => {
            let repositories = client::repositories(&ctx.paths).await?;
            let hint = "pass --workspace or run it in a registered checkout";
            let repository = super::issues::current_repository(ctx, repositories, hint).await?;
            Ok((repository.into(), None))
        }
        Err(error) => Err(error),
    }
}

/// The agent the child of `parent` starts with the requested `model`: the
/// named one, or with `fork` the parent's own from a fork of its session.
/// One that cannot start, or its detached launcher, fails before any work is
/// created.
async fn child_agent(
    ctx: &Context,
    parent: &str,
    agent: Option<Agent>,
    fork: bool,
    model: ModelChoice,
) -> Result<Option<ChildAgent>> {
    let session = if fork {
        Some(parent_session(ctx, parent).await?)
    } else {
        None
    };
    let agent = match (agent, &session) {
        (Some(agent), _) => agent,
        (None, Some(session)) => session.agent.parse()?,
        (None, None) => return Ok(None),
    };
    let settings = client::settings(&ctx.paths, ConfigTarget::Workspace(parent.to_owned())).await?;
    agents::ensure_available(agent.name(), &settings)?;
    terminal::ensure_background(&settings)?;
    if session.is_some() {
        agents::ensure_forks(&settings, agent.name())?;
    }
    let resolved = agents::resolve_model(&settings, agent.name(), model.clone());
    agents::check_model(&settings, agent.name(), &resolved)?;
    Ok(Some(ChildAgent {
        agent,
        model,
        fork: session.map(|session| session.session),
    }))
}

/// The session of the agent in `parent` that a fork starts from.
async fn parent_session(ctx: &Context, parent: &str) -> Result<AgentSession> {
    let method = AgentMethod::AgentSession {
        workspace: parent.to_owned(),
    };
    request(&ctx.paths, method).await
}

/// Split the child of `parent` on a new branch named `name`, assigned
/// `issue`, and start preparing it.
pub(super) async fn start_child(
    ctx: &Context,
    parent: String,
    name: String,
    base: Option<String>,
    issue: Option<&Issue>,
    options: CreationOptions,
) -> Result<StartedWorkspace> {
    let method = OperationMethod::StartSplit {
        workspace: parent,
        name,
        base,
        options,
        issue: issue.map(|issue| issue.url.clone()),
        agent: None,
        env: Some(crate::env::without_secrets()),
    };
    request(&ctx.paths, method).await
}

/// Run the child's setup and post-setup hook; `None` when setup failed.
pub(super) async fn prepare(ctx: &Context, child: Workspace) -> Result<Option<Workspace>> {
    if child.state != WorkspaceState::Preparing {
        super::workspaces::run_post_setup(ctx, &child).await?;
        return Ok(Some(child));
    }
    match super::workspaces::setup_workspace(ctx, &child, true).await? {
        Some(set_up) => set_up.finish(ctx).await.map(Some),
        None => Ok(None),
    }
}

/// Follow the preparation the daemon started for a workspace a request
/// opened to its end; `None` when setup failed.
pub(super) async fn follow_preparation(
    ctx: &Context,
    started: StartedWorkspace,
) -> Result<Option<Workspace>> {
    match super::workspaces::follow_setup(ctx, started).await? {
        Some(set_up) => set_up.finish(ctx).await.map(Some),
        None => Ok(None),
    }
}

/// Queue `agent` in `child`, prompted with `issue` or `args`; the daemon
/// starts it outside this process, so it gets the child's own scope.
async fn start_agent(
    ctx: &Context,
    child: &Workspace,
    ChildAgent { agent, model, fork }: ChildAgent,
    issue: Option<&Issue>,
    args: Vec<OsString>,
) -> Result<QueueEntry> {
    let plan = AddPlan::agent_in(child, agent.clone(), model, issue, args, fork);
    let title = issue.map(|issue| issue.title.clone());
    super::agent_queue::queue_in(ctx, &plan, child, String::from(agent), title).await
}

/// Report the child and its agent, then enter the child when this terminal
/// follows new workspaces and no agent works there.
fn report(ctx: &Context, child: &Workspace, agent: Option<&QueueEntry>) -> Result<()> {
    let palette = Palette::stdout(ctx.json);
    let mut text = format!(
        "Created {} {}",
        palette.paint(Style::Heading, &child.name),
        super::workspaces::location_phrase(child),
    );
    if let Some(entry) = agent {
        text.push_str(&match entry.state {
            QueueState::Queued => format!(
                "\nQueued {} (position {}); it starts when an agent slot is free",
                entry.agent,
                entry.position.unwrap_or(1)
            ),
            _ => format!("\nStarting {} in {}", entry.agent, child.name),
        });
    }
    ctx.emit(
        &text,
        Child {
            workspace: child.clone(),
            agent: agent.cloned(),
        },
    )?;
    if agent.is_none() && terminal::follows_caller(ctx) && !crate::env::is_scoped() {
        shell::navigate(&child.path, ctx.json)?;
    }
    Ok(())
}

/// The issue `input` names in the parent workspace's repository.
async fn load_issue(ctx: &Context, parent: &str, input: &str) -> Result<Issue> {
    let workspace = client::workspaces(&ctx.paths)
        .await?
        .into_iter()
        .find(|workspace| workspace.id == parent || workspace.name == parent)
        .ok_or_else(|| Refusal::new(Reason::NotFound, format!("unknown workspace: {parent}")))?;
    let repository = client::repositories(&ctx.paths)
        .await?
        .into_iter()
        .find(|repository| repository.id == workspace.repository_id)
        .context("the parent workspace's repository is not registered")?;
    super::issues::load(ctx, &repository, input).await
}

/// The parent and child spaces of a space, when it has any.
pub(super) fn render(parent: &Option<SpaceRef>, children: &[SpaceRef]) {
    if let Some(parent) = parent {
        outln!("Parent space:  {}", super::base::describe_space(parent));
    }
    if !children.is_empty() {
        let children: Vec<_> = (children.iter()).map(super::base::describe_space).collect();
        outln!("Child spaces:  {}", children.join(", "));
    }
}
