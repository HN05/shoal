//! Opening a planned workspace, setting it up and launching its agent.
use crate::client::space_ids::SpaceId;
use std::ffi::OsString;

use anyhow::{Context as _, Result};

use super::{
    place::{HereLaunch, launch_placed},
    queue::failing_entry,
    source::load_planned_issue,
    target::AddTarget,
    waiting::{AfterSetup, follow_add_setup},
};
use crate::client::preparation::{self, NewWorkspace, Preparing, Unprepared};
use crate::client::space_names::SpaceSelector;
use crate::client::{agent::AgentSessionId, queue::QueueEntryId};
use crate::{
    cli::{
        agents,
        commands::{
            issues, operations, queued_agents,
            workspaces::{SetUp, waiting::WaitingAgent},
        },
        context::Context,
        output::{Palette, Style},
        shell, terminal,
    },
    forge::items::reference::ItemInput,
    model::{QueueEntry, Workspace},
    paths::CommandPaths,
    protocol::{
        ConfigTarget,
        operations::{Preparation, Prepared, QueuedLaunch, WorkspaceStart},
    },
    start_plan::{AddPlan, Creation, ResolvedAddAgent},
    state::QueueState,
};

pub(in crate::cli) async fn execute_add(ctx: &Context, plan: AddPlan) -> Result<i32> {
    let target = AddTarget {
        selector: plan.repository.clone(),
        repositories: tokio::sync::OnceCell::new(),
    };
    let issue = load_planned_issue(ctx, &target, &plan).await;
    // A queued start reports a failed lookup as it reports any other failure.
    let issue = failing_entry(ctx, plan.queue_entry.as_ref(), issue).await?;
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
    entry: &QueueEntryId,
    start: impl std::future::Future<Output = Result<T>>,
) -> Result<T> {
    let _hold = queued_agents::Hold::start(&ctx.paths, entry).await;
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
        narrowing,
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
    let own = queued_launch(&agent, issue.as_ref(), (&mode, narrowing), told, false)?;
    let (launch, placed) = match start {
        AgentStart::Queued { now } => (
            queued_launch(&agent, issue.as_ref(), (&mode, narrowing), told, now)?,
            None,
        ),
        AgentStart::Placed { now } => (
            None,
            queued_launch(&agent, issue.as_ref(), (&mode, narrowing), told, now)?,
        ),
        AgentStart::Here => (None, None),
    };
    // The agent this start runs itself, which its preparation leaves
    // waiting in the queue should setup fail.
    let has_agent = own.is_some();
    // A start that names its workspace reports only its agent.
    let named = workspace.is_some();
    let start = AddStart {
        creation,
        parent: parent.or(chat.map(SpaceSelector::Id)),
        workspace,
        agent: launch.clone(),
        queue_entry: queue_entry.clone(),
        waiting: own.filter(|_| launch.is_none()),
    };
    // The daemon gave the starting entry the workspace before its setup.
    let preparing = start_add_workspace(ctx, &target, start, issue.as_ref()).await?;
    let opened = preparing.workspace();
    let (branch, name) = (opened.branch_or_name(), &opened.name);
    let label = match place_name {
        Some(place) => place.render(branch, name),
        None => name.to_string(),
    };
    // A placed agent may still be queued, so this place becomes the
    // workspace's only once it launches here.
    if launch.is_none() && placed.is_none() {
        let with_agent = agent.agent.is_some();
        terminal::host_workspace(ctx, opened, with_agent, &label).await?;
    }
    let (name, number) = (opened.name.clone(), opened.number);
    let subject = (issue.as_ref().map(issues::Issue::subject))
        .or(prompt.map(crate::config::templates::Subject::told));
    // Preparation leaves the agent it queues waiting itself.
    let waiting = has_agent.then(|| WaitingAgent::new(queue_entry.clone()));
    let mut rest = AfterSetup {
        pr,
        agent: launch.clone(),
        waiting,
    };
    let finished = finish_add_workspace(ctx, preparing, &mut rest, &agent, named).await?;
    // A failed setup frees the entry's slot, leaving the agent waiting.
    let (queue_entry, waits) = match rest.waiting {
        Some(waiting) => (waiting.starting.clone(), waiting.failed()),
        None => (queue_entry, false),
    };
    let Some(Prepared {
        workspace,
        agent: entry,
    }) = finished
    else {
        if let Some(entry) = &queue_entry {
            let error = anyhow::anyhow!(
                "setup of {name} did not complete; retry it with `shoal setup {number}` or skip it with `shoal setup {number} --skip`"
            );
            queued_agents::fail(ctx, entry, &error).await;
        }
        return Ok(1);
    };
    if waits {
        return report_waiting_in(ctx, &workspace).await;
    }
    if let Some(entry) = &queue_entry {
        crate::env::launch_queue_entry(entry);
    }
    if launch.is_some() {
        let entry = entry.context("preparation queued no agent")?;
        return report_queued_in(ctx, &workspace, &entry);
    }
    let opening = agents::Opening::New {
        mode: mode.unwrap_or(crate::config::modes::ModeName::DEFAULT),
        subject,
    };
    let here = HereLaunch {
        agent,
        opening,
        narrowing,
        args,
        fork,
    };
    match placed {
        Some(placed) => Box::pin(launch_placed(ctx, *workspace, *placed, here, &label)).await,
        None => here.run(ctx, *workspace).await,
    }
}

/// The launch of `agent` in `mode` with its narrowing that preparing the
/// added workspace queues, prompted with `issue` or else the plan's own prompt, with its
/// arguments and fork, at once regardless of the limits when `now`. The
/// daemon starts it outside this process, so it gets the workspace's own
/// scope.
pub(super) fn queued_launch(
    agent: &ResolvedAddAgent,
    issue: Option<&issues::Issue>,
    (mode, narrowing): (
        &Option<crate::config::modes::ModeName>,
        crate::config::permissions::PermissionsConfig,
    ),
    (prompt, args, fork): (&Option<String>, &[OsString], &Option<AgentSessionId>),
    now: bool,
) -> Result<Option<Box<QueuedLaunch>>> {
    let Some(chosen) = &agent.agent else {
        return Ok(None);
    };
    Ok(Some(Box::new(QueuedLaunch {
        agent: chosen.clone(),
        prompt: prompt.clone(),
        mode: mode.clone(),
        narrowing,
        model: agent.model.clone(),
        args: super::plan::launch_args(args.to_vec())?,
        fork: fork.clone(),
        issue: issue.map(issues::Issue::launch_issue),
        env: Some(crate::terminal::start_environment()),
        now,
    })))
}

/// Report the agent that waited in the queue for the setup of the added
/// `workspace`, which the queue starts.
async fn report_waiting_in(ctx: &Context, workspace: &Workspace) -> Result<i32> {
    let entries = queued_agents::entries(ctx).await?;
    let waiting = entries
        .iter()
        .find(|entry| entry.opens.as_ref() == Some(&workspace.id));
    match waiting {
        Some(entry) => report_queued_in(ctx, workspace, entry),
        None => Ok(0),
    }
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
            queued_agents::starts_when(entry)
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
    let settings = crate::config::settings(
        &ctx.paths,
        ConfigTarget::Space(SpaceSelector::id(&workspace.id)),
    )
    .await?;
    Ok(!agents::runs_in_session(chosen, &settings))
}

/// What the planned workspace opens: the recorded `workspace` a launch
/// goes to, or else what `creation` makes below `parent`, or for the user
/// without one.
struct AddStart {
    creation: Creation,
    parent: Option<SpaceSelector>,
    workspace: Option<SpaceId>,
    /// The agent its preparation queues.
    agent: Option<Box<QueuedLaunch>>,
    /// The starting queue entry whose launch opens it.
    queue_entry: Option<QueueEntryId>,
    /// The agent the caller starts itself, which waits should setup fail.
    waiting: Option<Box<QueuedLaunch>>,
}

/// Open the planned workspace, assigned `issue` unless the start names the
/// workspace, which keeps the issue it has, and follow its preparation.
async fn start_add_workspace<'c>(
    ctx: &'c Context,
    target: &AddTarget,
    AddStart {
        creation,
        parent,
        workspace,
        agent,
        queue_entry,
        waiting,
    }: AddStart,
    issue: Option<&issues::Issue>,
) -> Result<Preparing<'c, CommandPaths>> {
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
        typed,
    } = creation;
    let issue = issue.filter(|_| workspace.is_none());
    let start = match (workspace, pull_head, existing, copy) {
        (Some(workspace), ..) => WorkspaceStart::Workspace(SpaceSelector::Id(workspace)),
        (None, Some(head), ..) => WorkspaceStart::PullHead {
            url: match &pr {
                Some(ItemInput::Url(url)) => url.clone(),
                _ => anyhow::bail!("a PR head needs its PR's URL"),
            },
            head,
        },
        (None, None, Some(branch), _) => WorkspaceStart::Branch(branch),
        (None, None, None, Some(copy)) => WorkspaceStart::Copy {
            source: copy.source,
            branch,
            typed,
        },
        (None, None, None, None) => {
            let branch = branch.context("a branch name is required")?;
            match typed {
                true => WorkspaceStart::Typed(branch),
                false => WorkspaceStart::Name(branch),
            }
        }
    };
    let new = NewWorkspace {
        repository: target.selector.clone(),
        start,
        base,
        path,
        git_profile,
        parent,
        options,
        issue: issue.map(|issue| issue.url.clone().into()),
        pr,
        agent: agent.map(|agent| Box::new((*agent).into())),
        queue_entry,
        waiting,
    };
    let env = crate::env::without_secrets();
    let preparing = preparation::create(&ctx.paths, new, env);
    ctx.progress("Opening workspace", preparing).await
}

/// Follow the preparation of the started workspace, which sets it up, links
/// its PR and queues its agent, before reporting it, unless the start
/// `named` it, when only `--json` reports it, and entering it unless `agent`
/// takes the terminal.
async fn finish_add_workspace(
    ctx: &Context,
    preparing: Preparing<'_, CommandPaths>,
    rest: &mut AfterSetup,
    agent: &ResolvedAddAgent,
    named: bool,
) -> Result<Option<Prepared>> {
    let reused = preparing.reused();
    let Some(SetUp {
        workspace,
        post_setup,
        agent: entry,
    }) = follow_add_setup(ctx, preparing, rest).await?
    else {
        return Ok(None);
    };
    // An agent left waiting by a failed setup starts from the queue.
    let waits = rest.waiting.as_ref().is_some_and(WaitingAgent::failed);
    if waits {
        rest.agent = None;
    }
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
    ctx.show(workspace, |workspace| {
        if !named {
            outln!(
                "{} {} {}",
                if reused { "Opened" } else { "Created" },
                Palette::stdout(ctx.json).paint(Style::Heading, &workspace.name),
                super::super::location_phrase(workspace),
            );
        }
    })?;
    let starts = (post_setup.is_ok() && !waits).then_some(agent);
    if enters_added(ctx, workspace, starts).await? {
        shell::navigate(ctx, &workspace.path)?;
    }
    post_setup?;
    Ok(Some(prepared))
}

/// Run what preparation leaves after setup once the user ignored its
/// failure: `rest`, with the post-setup hook unless the workspace was
/// `reused` and no agent waits for its setup, which the hook lets start.
/// Returns the workspace with its PR and agent, and how the hook went.
async fn prepare_after_setup(
    ctx: &Context,
    workspace: Workspace,
    reused: bool,
    rest: &mut AfterSetup,
) -> Result<(Prepared, Result<()>)> {
    let (pr, agent) = (rest.pr.take(), rest.agent.take());
    let waits = rest.waiting.as_ref().is_some_and(WaitingAgent::failed);
    let steps = Preparation {
        post_setup: !reused || waits,
        pr,
        agent,
        ..Default::default()
    };
    let env = crate::env::without_secrets();
    let preparing = preparation::prepare(&ctx.paths, &workspace, steps, env).await?;
    let ended = operations::follow_preparation(ctx, preparing).await?;
    Ok(match ended {
        Ok(prepared) => (prepared, Ok(())),
        Err(Unprepared { workspace, failure }) => {
            let agent = None;
            let error = crate::operations::error(failure);
            (Prepared { workspace, agent }, Err(error))
        }
    })
}
