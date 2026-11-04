//! `shoal split`: a child workspace on a new branch, which the parent
//! workspace's agent works on and receives the activity of, or which an agent
//! of its own works on, which may start from a fork of the parent agent's
//! session; or a workspace at a commit, with no branch or agent of its own.
use std::ffi::OsString;

use anyhow::Result;

use crate::client::{operations::NewSplit, output::split::Child};
use crate::{
    agent::{Agent, ModelChoice},
    cli::{
        client::{self},
        context::Context,
        output::{Palette, Style},
        terminal,
        ui::{self, Fallback},
    },
    daemon::refusal::{Reason, Refusal},
    model::{AgentSession, QueueEntry, SpaceRef, Workspace},
    protocol::{
        CommitStart, ConfigTarget, CreationOptions, ItemContext,
        operations::{Prepared, QueuedLaunch, StartedWorkspace},
    },
    shell,
    state::{QueueState, WorkspaceState},
};

use super::issues::Issue;

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
    let requested = (split.agent, split.fork, split.model, split.args);
    let mut agent = child_agent(ctx, &parent, requested).await?;
    let issue = match &split.issue {
        Some(input) => Some(load_issue(ctx, &parent, input).await?),
        None => None,
    };
    if let (Some(agent), Some(issue)) = (&mut agent, &issue) {
        agent.issue = Some(issue.launch_issue());
    }
    let name = match (split.branch, &issue) {
        (Some(branch), _) => branch,
        (None, Some(issue)) => issue.branch_name(),
        (None, None) => anyhow::bail!("name the child's branch or pass --issue"),
    };
    let start = NewSplit {
        base: split.base,
        options: split.options,
        issue: issue.as_ref().map(|issue| issue.url.clone()),
        agent: agent.clone(),
        ..NewSplit::new(parent, name)
    };
    let child = start_child(ctx, start).await?;
    let Some(child) = follow_queuing(ctx, child, agent).await? else {
        return Ok(1);
    };
    report(ctx, &child.workspace, child.agent.as_ref())?;
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
    let parent = parent.or_else(|| ui::current_chat(&None));
    let start = CommitStart::Revision(base);
    let created =
        crate::client::workspaces::create_at_commit(&ctx.paths, repository, parent, start, options)
            .await?;
    let Some(child) = prepare(ctx, created).await? else {
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

/// The agent the child of `parent` starts with the requested model and
/// arguments: the named one, or with `fork` the parent's own from a fork of
/// its session. One that cannot start, or its detached launcher, fails
/// before any work is created.
async fn child_agent(
    ctx: &Context,
    parent: &str,
    (agent, fork, model, args): (Option<Agent>, bool, ModelChoice, Vec<OsString>),
) -> Result<Option<Box<QueuedLaunch>>> {
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
    let launch = Box::new(QueuedLaunch {
        agent: agent.into(),
        model,
        args: super::workspaces::launch_args(args)?,
        fork: session.map(|session| session.session),
        env: Some(terminal::start_environment()),
        ..Default::default()
    });
    let settings = client::settings(&ctx.paths, ConfigTarget::Workspace(parent.to_owned())).await?;
    super::check_queued_launch(&launch, &settings)?;
    terminal::ensure_background(&settings)?;
    Ok(Some(launch))
}

/// The session of the agent in `parent` that a fork starts from.
async fn parent_session(ctx: &Context, parent: &str) -> Result<AgentSession> {
    crate::client::agents::session(&ctx.paths, parent.to_owned()).await
}

/// Split the child `split` describes and start preparing it.
pub(super) async fn start_child(ctx: &Context, split: NewSplit) -> Result<StartedWorkspace> {
    let env = crate::env::without_secrets();
    crate::client::operations::start_split(&ctx.paths, split, env).await
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
    let prepared = follow_queuing(ctx, started, None).await?;
    Ok(prepared.map(|prepared| *prepared.workspace))
}

/// Follow the preparation as [`follow_preparation`] does, with the agent it
/// queued; `agent`, the one the request asked for, is queued after a setup
/// failure the user ignored, which ends preparation before it.
async fn follow_queuing(
    ctx: &Context,
    started: StartedWorkspace,
    agent: Option<Box<QueuedLaunch>>,
) -> Result<Option<Prepared>> {
    match super::workspaces::follow_setup(ctx, started).await? {
        Some(set_up) => set_up.finish_queuing(ctx, agent).await.map(Some),
        None => Ok(None),
    }
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
    let context = ItemContext::Workspace(parent.to_owned());
    super::issues::load(ctx, context, input).await
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
