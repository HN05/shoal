//! Opening a planned workspace, setting it up and launching its agent.
use std::ffi::OsString;

use anyhow::{Context as _, Result};

use super::{
    place::{HereLaunch, launch_placed},
    queue::failing_entry,
    source::load_add_issue,
    target::AddTarget,
};
use crate::client::operations::NewWorkspace;
use crate::client::space_names::SpaceSelector;
use crate::{
    agent_queue::{AddPlan, Creation, ResolvedAddAgent},
    cli::{
        agents,
        commands::{
            agent_queue, issues, operations,
            workspaces::{SetUp, follow_setup},
        },
        context::Context,
        output::{Palette, Style},
        shell, terminal,
    },
    model::{QueueEntry, Workspace},
    protocol::{
        ConfigTarget,
        operations::{
            OperationSpec, Preparation, Prepared, QueuedLaunch, StartedWorkspace, WorkspaceStart,
        },
    },
    state::QueueState,
};

pub(in crate::cli) async fn execute_add(ctx: &Context, plan: AddPlan) -> Result<i32> {
    let target = AddTarget {
        selector: plan.repository.clone(),
        repositories: tokio::sync::OnceCell::new(),
    };
    let issue = load_add_issue(ctx, &target, plan.issue.clone()).await;
    // A queued start reports a failed lookup as it reports any other failure.
    let issue = failing_entry(ctx, plan.queue_entry.as_deref(), issue).await?;
    execute_add_with_target(ctx, plan, target, issue).await
}

pub(super) async fn execute_add_with_target(
    ctx: &Context,
    plan: AddPlan,
    target: AddTarget,
    issue: Option<issues::Issue>,
) -> Result<i32> {
    let Some(entry) = plan.queue_entry.clone() else {
        return add_planned(ctx, plan, target, issue, AgentStart::Here).await;
    };
    let added = Box::pin(add_planned(ctx, plan, target, issue, AgentStart::Here));
    holding(ctx, &entry, added).await
}

/// Run `start` under queue `entry`'s slot, held until its agent takes it
/// over, freeing the slot with the error when `start` fails.
pub(super) async fn holding<T>(
    ctx: &Context,
    entry: &str,
    start: impl std::future::Future<Output = Result<T>>,
) -> Result<T> {
    let _hold = agent_queue::Hold::start(&ctx.paths, entry).await;
    failing_entry(ctx, Some(entry), start.await).await
}

/// Where the planned workspace's agent starts.
#[derive(Clone, Copy)]
pub(super) enum AgentStart {
    /// In this process.
    Here,
    /// Queued by the workspace's preparation, which the daemon starts
    /// detached, at once regardless of the limits when `now`.
    Queued { now: bool },
    /// Where the set-up workspace's settings put it: in this process when the
    /// agent runs its own way there, which leaves this process at once,
    /// otherwise queued as `Queued` once setup is done.
    Placed { now: bool },
}

/// Open the planned workspace and launch its agent where `start` says.
pub(super) async fn add_planned(
    ctx: &Context,
    plan: AddPlan,
    target: AddTarget,
    issue: Option<issues::Issue>,
    start: AgentStart,
) -> Result<i32> {
    let AddPlan {
        repository: _,
        creation,
        place_name,
        issue: _,
        agent,
        args,
        prompt,
        mode,
        fork,
        queue_entry,
        chat,
        parent,
        workspace,
    } = plan;
    let pr = creation.pr.clone();
    // A queued agent starts detached, so the caller's terminal, such as a
    // chat's session, stays its own.
    let told = (&prompt, args.as_slice(), &fork);
    let (launch, placed) = match start {
        AgentStart::Queued { now } => (
            queued_launch(&agent, issue.as_ref(), &mode, told, now)?,
            None,
        ),
        AgentStart::Placed { now } => (
            None,
            queued_launch(&agent, issue.as_ref(), &mode, told, now)?,
        ),
        AgentStart::Here => (None, None),
    };
    let start = AddStart {
        creation,
        parent: parent.or(chat),
        workspace,
        agent: launch.clone(),
    };
    let started = start_add_workspace(ctx, &target, start, issue.as_ref()).await?;
    if let Some(entry) = &queue_entry {
        agent_queue::attach(ctx, entry, &started.workspace).await?;
    }
    let (branch, title) = (started.workspace.branch_or_name(), &started.workspace.title);
    let label = match (place_name, &issue, title) {
        (Some(name), _, title) => name.render(branch, title.as_deref()),
        (None, _, Some(title)) => title.clone(),
        (None, Some(issue), None) => issue.tab_label(target.repository(ctx).await?),
        (None, None, None) => branch.to_owned(),
    };
    // A placed agent may still be queued, so this place becomes the
    // workspace's only once it launches here.
    if launch.is_none() && placed.is_none() {
        let with_agent = agent.agent.is_some();
        terminal::host_workspace(ctx, &started.workspace, with_agent, &label).await?;
    }
    let name = started.workspace.name.clone();
    let subject = (issue.as_ref().map(issues::Issue::subject))
        .or(prompt.map(crate::config::templates::Subject::told));
    let rest = AfterSetup {
        pr,
        agent: launch.clone(),
    };
    let finished = finish_add_workspace(ctx, started, rest, &agent).await?;
    let Some(Prepared {
        workspace,
        agent: entry,
    }) = finished
    else {
        if let Some(entry) = &queue_entry {
            let error = anyhow::anyhow!(
                "setup of {name} did not complete; retry it with `shoal work setup {name}` or skip it with `shoal work setup {name} --skip`"
            );
            agent_queue::fail(ctx, entry, &error).await;
        }
        return Ok(1);
    };
    if let Some(entry) = &queue_entry {
        crate::env::launch_queue_entry(entry);
    }
    if launch.is_some() {
        let entry = entry.context("preparation queued no agent")?;
        return report_queued_in(ctx, &workspace, &entry);
    }
    let opening = agents::Opening::New {
        mode: mode.unwrap_or_else(|| crate::config::modes::DEFAULT.to_owned()),
        subject,
    };
    let here = HereLaunch {
        agent,
        opening,
        args,
        fork,
    };
    match placed {
        Some(placed) => Box::pin(launch_placed(ctx, *workspace, *placed, here, &label)).await,
        None => here.run(ctx, *workspace).await,
    }
}

/// The launch of `agent` in `mode` that preparing the added workspace
/// queues, prompted with `issue` or else the plan's own prompt, with its
/// arguments and fork, at once regardless of the limits when `now`. The
/// daemon starts it outside this process, so it gets the workspace's own
/// scope.
fn queued_launch(
    agent: &ResolvedAddAgent,
    issue: Option<&issues::Issue>,
    mode: &Option<String>,
    (prompt, args, fork): (&Option<String>, &[OsString], &Option<String>),
    now: bool,
) -> Result<Option<Box<QueuedLaunch>>> {
    let Some(chosen) = &agent.agent else {
        return Ok(None);
    };
    Ok(Some(Box::new(QueuedLaunch {
        agent: chosen.clone().into(),
        prompt: prompt.clone(),
        mode: mode.clone(),
        model: agent.model.clone(),
        args: super::plan::launch_args(args.to_vec())?,
        fork: fork.clone(),
        issue: issue.map(issues::Issue::launch_issue),
        env: Some(crate::terminal::start_environment()),
        now,
    })))
}

/// Report the agent queued in the added `workspace`.
pub(super) fn report_queued_in(
    ctx: &Context,
    workspace: &Workspace,
    entry: &QueueEntry,
) -> Result<i32> {
    let text = match entry.state {
        QueueState::Queued => format!(
            "Queued {} in {} (position {}); it starts {}",
            entry.agent,
            workspace.name,
            entry.position.unwrap_or(1),
            agent_queue::starts_when(entry)
        ),
        _ => format!("Starting {} in {}", entry.agent, workspace.name),
    };
    ctx.emit(&text, entry)?;
    Ok(0)
}

/// Whether the caller's shell enters the added `workspace`, where `agent`
/// starts unless `None`. An agent that runs in a session this terminal
/// attaches to leaves it where it was, as detaching returns to that shell
/// rather than to the workspace.
async fn enters_added(
    ctx: &Context,
    workspace: &Workspace,
    agent: Option<&ResolvedAddAgent>,
) -> Result<bool> {
    if !terminal::follows_caller(ctx) {
        return Ok(false);
    }
    let Some(chosen) = agent.and_then(|agent| agent.agent.as_ref()) else {
        return Ok(true);
    };
    let settings = crate::client::workspaces::settings(
        &ctx.paths,
        ConfigTarget::Workspace(SpaceSelector::id(&workspace.id)),
    )
    .await?;
    Ok(!agents::runs_in_session(chosen, &settings))
}

/// What the planned workspace opens: the recorded `workspace` a launch
/// goes to, or else what `creation` makes below `parent`, or for the user
/// without one.
struct AddStart {
    creation: Creation,
    parent: Option<String>,
    workspace: Option<String>,
    /// The agent its preparation queues.
    agent: Option<Box<QueuedLaunch>>,
}

/// Open the planned workspace, assigned `issue`, and start preparing it.
async fn start_add_workspace(
    ctx: &Context,
    target: &AddTarget,
    AddStart {
        creation,
        parent,
        workspace,
        agent,
    }: AddStart,
    issue: Option<&issues::Issue>,
) -> Result<StartedWorkspace> {
    let Creation {
        path,
        branch,
        existing,
        base,
        git_profile,
        pr,
        options,
        pull_head,
        copy,
    } = creation;
    let start = match (workspace, pull_head, existing, copy) {
        (Some(workspace), ..) => WorkspaceStart::Workspace(SpaceSelector::parse(&workspace)),
        (None, Some(head), ..) => WorkspaceStart::PullHead {
            url: pr.clone().context("a PR head needs its PR")?,
            head,
        },
        (None, None, Some(branch), _) => WorkspaceStart::Branch(branch),
        (None, None, None, Some(copy)) => WorkspaceStart::Copy {
            source: copy.source,
            branch,
        },
        (None, None, None, None) => {
            WorkspaceStart::Name(branch.context("a branch name is required")?)
        }
    };
    let new = NewWorkspace {
        repository: target.selector.clone(),
        start,
        base,
        path,
        git_profile,
        parent: parent.as_deref().map(SpaceSelector::parse),
        options,
        issue: issue.map(|issue| issue.url.clone()),
        pr,
        agent,
    };
    crate::client::operations::start_workspace(&ctx.paths, new, crate::env::without_secrets()).await
}

/// What preparing the added workspace does after setup: linking `pr`, then
/// the post-setup hook, then queuing `agent`.
struct AfterSetup {
    pr: Option<String>,
    agent: Option<Box<QueuedLaunch>>,
}

/// Follow the preparation of the started workspace, which sets it up, links
/// its PR and queues its agent, before reporting it and entering it unless
/// `agent` takes the terminal.
async fn finish_add_workspace(
    ctx: &Context,
    started: StartedWorkspace,
    rest: AfterSetup,
    agent: &ResolvedAddAgent,
) -> Result<Option<Prepared>> {
    let reused = started.reused;
    let Some(SetUp {
        workspace,
        post_setup,
        agent: entry,
    }) = follow_setup(ctx, started).await?
    else {
        return Ok(None);
    };
    // The post-setup hook may change how the agent launches, so the shell's
    // destination waits for it; a failed hook starts no agent.
    let (prepared, post_setup) = match post_setup {
        Some(result) => (
            Prepared {
                workspace: Box::new(workspace),
                agent: entry,
            },
            result,
        ),
        None => prepare_after_setup(ctx, workspace, reused, rest).await?,
    };
    let workspace = &prepared.workspace;
    ctx.emit(
        &format!(
            "{} {} {}",
            if reused { "Opened" } else { "Created" },
            Palette::stdout(ctx.json).paint(Style::Heading, &workspace.name),
            super::super::location_phrase(workspace),
        ),
        workspace,
    )?;
    let starts = post_setup.is_ok().then_some(agent);
    if enters_added(ctx, workspace, starts).await? {
        shell::navigate(&workspace.path, ctx.json)?;
    }
    post_setup?;
    Ok(Some(prepared))
}

/// Run what preparation leaves after setup once the user ignored its
/// failure: `rest`, with the post-setup hook unless the workspace was
/// `reused`. Returns the workspace with its PR and agent, and how the hook
/// went.
async fn prepare_after_setup(
    ctx: &Context,
    workspace: Workspace,
    reused: bool,
    AfterSetup { pr, agent }: AfterSetup,
) -> Result<(Prepared, Result<()>)> {
    let spec = OperationSpec::Prepare {
        workspace: SpaceSelector::id(&workspace.id),
        steps: Preparation {
            post_setup: !reused,
            pr,
            agent,
            ..Default::default()
        },
    };
    let (agent, result) = match operations::run::<Prepared>(ctx, spec).await {
        Ok(prepared) => (prepared.agent, Ok(())),
        Err(error) => (None, Err(error)),
    };
    let workspace =
        crate::client::workspaces::inspect(&ctx.paths, SpaceSelector::id(&workspace.id))
            .await?
            .workspace;
    let workspace = Box::new(workspace);
    Ok((Prepared { workspace, agent }, result))
}
