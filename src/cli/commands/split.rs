//! `shoal split`: a child workspace on a new branch, which the parent
//! workspace's agent works on and receives the activity of, or which an agent
//! of its own works on.
use std::ffi::OsString;

use anyhow::{Context as _, Result};
use serde::Serialize;

use crate::{
    agent::Agent,
    cli::{
        agents,
        client::{self, request},
        context::Context,
        output::{Palette, Style},
        terminal,
        ui::{self, Fallback},
    },
    model::{QueueEntry, Workspace},
    protocol::{ConfigTarget, ItemMethod, WorkspaceMethod},
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
    pub args: Vec<OsString>,
}

/// The ready child and the queue entry of its agent, when one was started.
#[derive(Serialize)]
struct Report<'a> {
    #[serde(flatten)]
    workspace: &'a Workspace,
    #[serde(skip_serializing_if = "Option::is_none")]
    agent: Option<&'a QueueEntry>,
}

pub(super) async fn run(ctx: &Context, workspace: Option<String>, split: Split) -> Result<i32> {
    // A supplied branch fails before the parent picker opens.
    if let Some(branch) = &split.branch {
        crate::git::check_branch_name(None, branch).await?;
    }
    let parent = ui::select_workspace(ctx, workspace, Fallback::CurrentDirectory).await?;
    // An agent that cannot start, or its detached launcher, fails before
    // any work is created.
    if let Some(agent) = &split.agent {
        let settings =
            client::settings(&ctx.paths, ConfigTarget::Workspace(parent.clone())).await?;
        agents::ensure_available(agent.name(), &settings)?;
        terminal::ensure_background(&settings)?;
    }
    let issue = match &split.issue {
        Some(input) => Some(load_issue(ctx, &parent, input).await?),
        None => None,
    };
    let name = match (split.branch, &issue) {
        (Some(branch), _) => branch,
        (None, Some(issue)) => issue.branch_name(),
        (None, None) => anyhow::bail!("name the child's branch or pass --issue"),
    };
    let child = create_child(ctx, parent, name, split.base, issue.as_ref()).await?;
    let Some(child) = prepare(ctx, child).await? else {
        return Ok(1);
    };
    let agent = match split.agent {
        Some(agent) => Some(start_agent(ctx, &child, agent, issue.as_ref(), split.args).await?),
        None => None,
    };
    report(ctx, &child, agent.as_ref())?;
    Ok(0)
}

/// Record the child of `parent` on a new branch named `name`, assigned `issue`.
pub(super) async fn create_child(
    ctx: &Context,
    parent: String,
    name: String,
    base: Option<String>,
    issue: Option<&Issue>,
) -> Result<Workspace> {
    let child: Workspace = request(
        &ctx.paths,
        WorkspaceMethod::SplitWorkspace {
            workspace: parent,
            name,
            base,
        },
    )
    .await?;
    if let Some(issue) = issue {
        request::<()>(
            &ctx.paths,
            ItemMethod::SetIssue {
                workspace: child.id.clone(),
                url: issue.url.clone(),
                title: Some(issue.title.clone()),
            },
        )
        .await?;
    }
    Ok(child)
}

/// Run the child's setup and post-setup hook; `None` when setup failed.
pub(super) async fn prepare(ctx: &Context, child: Workspace) -> Result<Option<Workspace>> {
    let child = match child.state {
        WorkspaceState::Preparing => match super::workspaces::setup_workspace(ctx, &child).await? {
            Some(ready) => ready,
            None => return Ok(None),
        },
        _ => child,
    };
    super::workspaces::run_post_setup(ctx, &child).await?;
    Ok(Some(child))
}

/// Queue `agent` in `child`, prompted with `issue` or `args`; the daemon
/// starts it outside this process, so it gets the child's own scope.
async fn start_agent(
    ctx: &Context,
    child: &Workspace,
    agent: Agent,
    issue: Option<&Issue>,
    args: Vec<OsString>,
) -> Result<QueueEntry> {
    let plan = AddPlan::agent_in(child, agent.clone(), issue, args);
    let title = issue.map(|issue| issue.title.clone());
    super::agent_queue::queue_in(ctx, &plan, child, String::from(agent), title).await
}

/// Report the child and its agent, then enter the child when this terminal
/// follows new workspaces and no agent works there.
fn report(ctx: &Context, child: &Workspace, agent: Option<&QueueEntry>) -> Result<()> {
    let palette = Palette::stdout(ctx.json);
    let mut text = format!(
        "Created {} on branch {} at {}",
        palette.paint(Style::Heading, &child.name),
        child.branch,
        child.path.display()
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
        Report {
            workspace: child,
            agent,
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
        .with_context(|| format!("unknown workspace: {parent}"))?;
    let repository = client::repositories(&ctx.paths)
        .await?
        .into_iter()
        .find(|repository| repository.id == workspace.repository_id)
        .context("the parent workspace's repository is not registered")?;
    super::issues::load(ctx, &repository, input).await
}

/// The parent and child workspaces, when the workspace has any.
pub(super) fn render(workspace: &Workspace) {
    if let Some(parent) = &workspace.parent_workspace {
        outln!("Parent workspace: {}", super::base::describe(parent));
    }
    if !workspace.child_workspaces.is_empty() {
        let children: Vec<_> = workspace
            .child_workspaces
            .iter()
            .map(super::base::describe)
            .collect();
        outln!("Child workspaces: {}", children.join(", "));
    }
}
