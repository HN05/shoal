//! Opening a recorded workspace for its start, following its setup.
use shoal_ids::space_ids::SpaceId;

use anyhow::Result;

use super::{
    target::AddTarget,
    waiting::{AfterSetup, follow_add_setup},
};
use crate::client::preparation::{self, NewWorkspace, Preparing, Unprepared};
use crate::{
    cli::{
        agents,
        commands::{
            operations, queued_agents, start,
            workspaces::{SetUp, waiting::WaitingAgent},
        },
        context::Context,
        output::{Palette, Style},
        shell, terminal,
    },
    client::connection::CommandPaths,
    forge::items::reference::ItemInput,
    model::Workspace,
    protocol::{
        ConfigTarget,
        operations::{Preparation, Prepared, QueuedLaunch, WorkspaceStart},
    },
    start_plan::{QueuedStart, ResolvedAddAgent},
    terminal::PlaceName,
};
use shoal_ids::queue_entry_ids::QueueEntryId;
use shoal_protocol::space_names::SpaceSelector;

/// Open the recorded workspace `space` in the place this process runs in,
/// named as `place_name` says, and give it a shell, as a start of it
/// without an agent does.
pub(in crate::cli::commands) async fn reopen_with_shell(
    ctx: &Context,
    space: &SpaceId,
    place_name: Option<PlaceName>,
) -> Result<i32> {
    let workspace = SpaceSelector::id(space);
    let workspace = crate::client::workspaces::inspect(&ctx.paths, workspace).await?;
    match open_recorded(ctx, &workspace.workspace, None, place_name).await? {
        Opened::Ready { workspace, .. } => terminal::open_shell(ctx, &workspace.path).await,
        Opened::Ended(code) => Ok(code),
    }
}

/// What opening a recorded workspace for its start left.
pub(in crate::cli::commands) enum Opened {
    /// The workspace, ready for the agent of the starting launch `entry`,
    /// if any, or for a shell.
    Ready {
        workspace: Box<Workspace>,
        entry: Option<QueueEntryId>,
    },
    /// The start ended with this code, as a failed setup ends it.
    Ended(i32),
}

/// Open the recorded `workspace` in the place this process runs in,
/// labelled as `place_name` says, for `agent`, the starting launch's entry
/// and its start, else for a shell, and follow its preparation, which links
/// the start's PR. A failed setup fails the entry, unless the agent waits
/// in the queue for a retried setup, which this reports.
pub(in crate::cli::commands) async fn open_recorded(
    ctx: &Context,
    workspace: &Workspace,
    agent: Option<(&QueueEntryId, &QueuedStart)>,
    place_name: Option<PlaceName>,
) -> Result<Opened> {
    let target = AddTarget {
        selector: workspace.repository().into(),
        repositories: tokio::sync::OnceCell::new(),
    };
    let queue_entry = agent.map(|(entry, _)| entry.clone());
    let pr = agent.and_then(|(_, start)| start.pr.clone());
    let start = AddStart {
        workspace: workspace.id.clone(),
        pr: pr.clone(),
        queue_entry: queue_entry.clone(),
        // The agent this start runs itself, which its preparation leaves
        // waiting in the queue should setup fail.
        waiting: agent.map(|(_, start)| Box::new(waiting_launch(&start.launch))),
    };
    // The daemon gave the starting entry the workspace before its setup.
    let preparing = start_add_workspace(ctx, &target, start).await?;
    let opened = preparing.workspace();
    let (branch, name) = (opened.branch_or_name(), &opened.name);
    let label = match place_name {
        Some(place) => place.render(branch, name),
        None => name.to_string(),
    };
    let owner = crate::terminal::Owner::Workspace(&opened.id);
    terminal::host_space(ctx, owner, agent.is_some(), &label).await?;
    let (name, number) = (opened.name.clone(), opened.number);
    // Preparation leaves the agent it queues waiting itself.
    let waiting = agent.map(|_| WaitingAgent::new(queue_entry.clone()));
    let mut rest = AfterSetup { pr, waiting };
    let resolved = ResolvedAddAgent {
        agent: agent.map(|(_, start)| start.launch.agent.clone()),
        model: Default::default(),
    };
    // The start names its workspace, so it reports only its agent.
    let finished = finish_add_workspace(ctx, preparing, &mut rest, &resolved, true).await?;
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
        return Ok(Opened::Ended(1));
    };
    if waits {
        return report_waiting_in(ctx, &workspace).await.map(Opened::Ended);
    }
    Ok(Opened::Ready {
        workspace,
        entry: queue_entry,
    })
}

/// `launch`, which its caller runs, as the queue starts it once it waits
/// for a retried setup: outside that caller, with the workspace's own
/// scope.
fn waiting_launch(launch: &QueuedLaunch) -> QueuedLaunch {
    QueuedLaunch {
        here: false,
        now: false,
        ..launch.clone()
    }
}

/// Report the agent that waited in the queue for the setup of the added
/// `workspace`, which the queue starts.
async fn report_waiting_in(ctx: &Context, workspace: &Workspace) -> Result<i32> {
    let entries = queued_agents::entries(ctx).await?;
    let waiting = entries
        .iter()
        .find(|entry| entry.opens.as_ref() == Some(&workspace.id));
    match waiting {
        Some(entry) => start::report_queued_in(ctx, &workspace.name, entry),
        None => Ok(0),
    }
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

/// The recorded `workspace` the planned start opens, linking `pr`.
struct AddStart {
    workspace: SpaceId,
    pr: Option<ItemInput>,
    /// The starting queue entry whose launch opens it.
    queue_entry: Option<QueueEntryId>,
    /// The agent the caller starts itself, which waits should setup fail.
    waiting: Option<Box<QueuedLaunch>>,
}

impl AddStart {
    /// The request that opens it in `target`'s repository, which keeps the
    /// issue it has.
    fn request(self, target: &AddTarget) -> NewWorkspace {
        let Self {
            workspace,
            pr,
            queue_entry,
            waiting,
        } = self;
        let start = WorkspaceStart::Workspace(SpaceSelector::Id(workspace));
        NewWorkspace {
            pr,
            queue_entry,
            waiting,
            ..NewWorkspace::new(target.selector.clone(), start)
        }
    }
}

/// Open the recorded workspace that `start` names and follow its
/// preparation.
async fn start_add_workspace<'c>(
    ctx: &'c Context,
    target: &AddTarget,
    start: AddStart,
) -> Result<Preparing<'c, CommandPaths>> {
    let new = start.request(target);
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
