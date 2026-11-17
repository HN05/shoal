//! Opening a planned workspace, setting it up and launching its agent.
use shoal_ids::space_ids::SpaceId;
use std::ffi::OsString;

use anyhow::Result;

use super::{
    source::load_planned_issue,
    target::AddTarget,
    waiting::{AfterSetup, follow_add_setup},
};
use crate::client::preparation::{self, NewWorkspace, Preparing, Unprepared};
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
    client::connection::CommandPaths,
    model::{QueueEntry, Workspace},
    protocol::{
        ConfigTarget,
        operations::{Preparation, Prepared, QueuedLaunch},
    },
    start_plan::{AddPlan, Creation, ResolvedAddAgent},
    state::QueueState,
};
use shoal_ids::{agent_session_ids::AgentSessionId, queue_entry_ids::QueueEntryId};
use shoal_protocol::launches::LaunchSource;
use shoal_protocol::space_names::SpaceSelector;

/// Carry out `plan` in this process: follow the preparation it names and
/// run its agent once the queue lets it, as a focused start's place does,
/// or else open what it names and launch its agent.
pub(in crate::cli) async fn execute_add(ctx: &Context, plan: AddPlan) -> Result<i32> {
    if plan.handoff.is_some() {
        return Box::pin(super::attach::run_in_place(ctx, plan)).await;
    }
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
        return add_planned(ctx, plan, target, issue).await;
    };
    let added = Box::pin(add_planned(ctx, plan, target, issue));
    holding(ctx, &entry, added).await
}

/// `result`, after failing the queue `entry`, if any, with its error.
async fn failing_entry<T>(
    ctx: &Context,
    entry: Option<&QueueEntryId>,
    result: Result<T>,
) -> Result<T> {
    if let (Some(entry), Err(error)) = (entry, &result) {
        crate::queued_starts::fail(&ctx.paths, entry, error).await;
    }
    result
}

/// The launch of the agent that starts in this process.
struct HereLaunch {
    agent: ResolvedAddAgent,
    opening: agents::Opening,
    /// The permissions `--permission` gives the agent, whatever the mode's.
    narrowing: crate::config::permissions::PermissionsConfig,
    args: Vec<OsString>,
    fork: Option<AgentSessionId>,
}

impl HereLaunch {
    /// Start the agent in `workspace`.
    async fn run(self, ctx: &Context, workspace: Workspace) -> Result<i32> {
        let Self {
            agent,
            opening,
            narrowing,
            args,
            fork,
        } = self;
        let launch = super::agent::Launch {
            opening,
            narrowing,
            args,
            fork,
        };
        super::agent::launch(ctx, agent, workspace, launch).await
    }
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

/// Open the planned workspace and launch its agent here.
async fn add_planned(
    ctx: &Context,
    plan: AddPlan,
    target: AddTarget,
    issue: Option<issues::Issue>,
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
        handoff: _,
    } = plan;
    let pr = creation.pr.clone();
    let told = (&prompt, args.as_slice(), &fork);
    let own = queued_launch(&agent, issue.as_ref(), (&mode, narrowing), told)?;
    // The agent this start runs itself, which its preparation leaves
    // waiting in the queue should setup fail.
    let has_agent = own.is_some();
    // A start that names its workspace reports only its agent.
    let named = workspace.is_some();
    let start = AddStart {
        creation,
        parent: parent.or(chat.map(SpaceSelector::Id)),
        workspace,
        queue_entry: queue_entry.clone(),
        waiting: own,
    };
    // The daemon gave the starting entry the workspace before its setup.
    let preparing = start_add_workspace(ctx, &target, start, issue.as_ref()).await?;
    let opened = preparing.workspace();
    let (branch, name) = (opened.branch_or_name(), &opened.name);
    let label = match place_name {
        Some(place) => place.render(branch, name),
        None => name.to_string(),
    };
    let with_agent = agent.agent.is_some();
    terminal::host_workspace(ctx, opened, with_agent, &label).await?;
    let (name, number) = (opened.name.clone(), opened.number);
    let subject = (issue.as_ref().map(issues::Issue::subject))
        .or(prompt.map(crate::config::templates::Subject::told));
    // Preparation leaves the agent it queues waiting itself.
    let waiting = has_agent.then(|| WaitingAgent::new(queue_entry.clone()));
    let mut rest = AfterSetup { pr, waiting };
    let finished = finish_add_workspace(ctx, preparing, &mut rest, &agent, named).await?;
    // A failed setup frees the entry's slot, leaving the agent waiting.
    let (queue_entry, waits) = match rest.waiting {
        Some(waiting) => (waiting.starting.clone(), waiting.failed()),
        None => (queue_entry, false),
    };
    let Some(Prepared { workspace, .. }) = finished else {
        if let Some(entry) = &queue_entry {
            let error = anyhow::anyhow!(
                "setup of {name} did not complete; retry it with `shoal setup {number}` or skip it with `shoal setup {number} --skip`"
            );
            crate::queued_starts::fail(&ctx.paths, entry, &error).await;
        }
        return Ok(1);
    };
    if waits {
        return report_waiting_in(ctx, &workspace).await;
    }
    if let Some(entry) = &queue_entry {
        crate::env::launch_queue_entry(entry);
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
    here.run(ctx, *workspace).await
}

/// The launch of `agent` in `mode` with its narrowing that preparing the
/// added workspace leaves waiting in the queue should its setup fail,
/// prompted with `issue` or else the plan's own prompt, with its arguments
/// and fork. The daemon then starts it outside this process, so it gets
/// the workspace's own scope.
fn queued_launch(
    agent: &ResolvedAddAgent,
    issue: Option<&issues::Issue>,
    (mode, narrowing): (
        &Option<crate::config::modes::ModeName>,
        crate::config::permissions::PermissionsConfig,
    ),
    (prompt, args, fork): (&Option<String>, &[OsString], &Option<AgentSessionId>),
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
        now: false,
        here: false,
        source: Some(LaunchSource::Manual),
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
    let settings = shoal_workers::settings::of(
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
    /// The starting queue entry whose launch opens it.
    queue_entry: Option<QueueEntryId>,
    /// The agent the caller starts itself, which waits should setup fail.
    waiting: Option<Box<QueuedLaunch>>,
}

impl AddStart {
    /// The request that opens it in `target`'s repository, assigned
    /// `issue` unless it names the workspace, which keeps the issue it has.
    fn request(self, target: &AddTarget, issue: Option<&issues::Issue>) -> Result<NewWorkspace> {
        let Self {
            creation,
            parent,
            workspace,
            queue_entry,
            waiting,
        } = self;
        let issue = issue.filter(|_| workspace.is_none());
        let crate::start_plan::Opening {
            start,
            base,
            path,
            git_profile,
            options,
            pr,
        } = creation.opening(workspace)?;
        Ok(NewWorkspace {
            repository: target.selector.clone(),
            start,
            base,
            path,
            git_profile,
            parent,
            options,
            issue: issue.map(|issue| issue.url.clone().into()),
            pr,
            agent: None,
            queue_entry,
            waiting,
            blockers: Vec::new(),
        })
    }
}

/// Open the planned workspace that `start` describes, assigned `issue`
/// unless the start names the workspace, and follow its preparation.
async fn start_add_workspace<'c>(
    ctx: &'c Context,
    target: &AddTarget,
    start: AddStart,
    issue: Option<&issues::Issue>,
) -> Result<Preparing<'c, CommandPaths>> {
    let new = start.request(target, issue)?;
    let env = crate::env::without_secrets();
    let preparing = preparation::create(&ctx.paths, new, env);
    ctx.progress("Opening workspace", preparing).await
}

/// Follow the preparation of the started workspace, which sets it up, links
/// its PR and queues its agent, before reporting it, unless the start
/// `named` it, which reports only its agent, and entering it unless `agent`
/// takes the terminal.
pub(super) async fn finish_add_workspace(
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
    if !named {
        ctx.show(workspace, |workspace| {
            outln!(
                "{} {} {}",
                if reused { "Opened" } else { "Created" },
                Palette::stdout(ctx.json).paint(Style::Heading, &workspace.name),
                super::super::location_phrase(workspace),
            );
        })?;
    }
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
    let waits = rest.waiting.as_ref().is_some_and(WaitingAgent::failed);
    let steps = Preparation {
        post_setup: !reused || waits,
        pr: rest.pr.take(),
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
