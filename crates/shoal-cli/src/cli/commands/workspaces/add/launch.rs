//! A detached start the daemon carries out: once this process has picked
//! what a terminal picks, it sends the start as one [`Launch`] and reports
//! what the daemon started, following the setup of a workspace it opened.
use std::ffi::OsString;

use anyhow::Result;

use super::{
    AddOptions,
    agent::AddSettings,
    detached::{ensure_detachable, unplaceable},
    finish::{finish_add_workspace, report_queued_in},
    mode::choose_add_mode,
    plan::launch_args,
    source::pick_add_source,
    target::resolve_add_target,
    waiting::AfterSetup,
};
use crate::{
    cli::{
        agents,
        commands::{chat, queued_agents},
        context::Context,
        ui::ModeChoice,
    },
    config::Effective,
    model::Workspace,
    protocol::operations::{Prepared, StartedWorkspace},
    start_plan::{Creation, ResolvedAddAgent},
};
use shoal_protocol::launches::{
    IssueChoice, Launch, Launched, QueuedWorker, SpaceChoice, StartTarget,
};
use shoal_protocol::space_names::SpaceSelector;

/// Have the daemon start `agent` as the worker of `workspace`, in `mode`,
/// as `options` ask, with `args` as its task, and report it.
pub(super) async fn launch_worker(
    ctx: &Context,
    workspace: &Workspace,
    (mode, agent): (Option<crate::config::modes::ModeName>, ResolvedAddAgent),
    args: Vec<OsString>,
    (options, settings): (AddOptions, &AddSettings<'_>),
) -> Result<i32> {
    ensure_detachable(settings.get().await?, agent.agent.as_ref(), &options)?;
    let launch = Launch {
        in_workspace: Some(SpaceChoice::Space(SpaceSelector::id(&workspace.id))),
        mode,
        agent: agent.agent,
        model: agent.model,
        fork: options.fork,
        permissions: options.narrowing,
        blockers: options.blockers,
        now: options.now,
        task: launch_args(args)?,
        env: Some(crate::terminal::start_environment()),
        ..Default::default()
    };
    send(ctx, launch, None).await
}

/// Have the daemon start `launch` in a new workspace, in the mode `mode`
/// names or, on a terminal, picks, once a terminal has picked what the
/// launch leaves to a picker, refusing what only a terminal can show.
/// The repository's `settings`, or the global config's without one, refuse
/// an agent they cannot start detached when no terminal picks the rest.
pub(in crate::cli::commands) async fn launch_workspace(
    ctx: &Context,
    mut launch: Launch,
    mode: ModeChoice,
    (options, settings): (&AddOptions, &Effective),
) -> Result<i32> {
    let picked = pick_missing(ctx, &mut launch).await?;
    launch.mode = match &picked {
        Some(picked) => choose_add_mode(ctx, picked, Some(mode)).await?,
        None => mode.named(),
    };
    let settings = match &picked {
        Some(picked) => picked.get().await?,
        None => settings,
    };
    let agent = starting_agent(&launch, settings);
    launch.unplaceable = unplaceable(settings, agent.as_ref(), options)?;
    send(ctx, launch, agent).await
}

/// The agent `launch` starts under `settings`: the one it names, else for a
/// start on a positional issue `agent.default`.
fn starting_agent(launch: &Launch, settings: &Effective) -> Option<crate::agent::Agent> {
    match (&launch.agent, issue_default(launch)) {
        (Some(agent), _) => Some(agent.clone()),
        (None, true) => settings.agent.default.clone(),
        (None, false) => None,
    }
}

/// Pick on a terminal what `launch` leaves to a picker there: its
/// repository, what it starts from and its agent, returning the settings of
/// the repository it starts in. Without a terminal the daemon decides.
async fn pick_missing<'a>(
    ctx: &'a Context,
    launch: &mut Launch,
) -> Result<Option<AddSettings<'a>>> {
    if !ctx.interactive() {
        return Ok(None);
    }
    let named = (launch.repository.clone()).or_else(|| match &launch.target {
        Some(StartTarget::Repository(named)) => Some(named.clone()),
        _ => None,
    });
    let issue = launch.issue.as_ref().and_then(IssueChoice::named).cloned();
    let named = named.map(crate::repository::named).transpose()?;
    let target = resolve_add_target(ctx, named, issue.as_ref()).await?;
    launch.repository = Some(target.repository(ctx).await?.id.to_string());
    if matches!(launch.target, Some(StartTarget::Repository(_))) {
        launch.target = None;
    }
    if names_nothing(launch) {
        let mut creation = Creation::default();
        let picked = pick_add_source(ctx, &target, &mut creation).await?;
        picked_source(launch, creation, picked);
    }
    let settings = AddSettings::new(ctx, &target.selector);
    if launch.agent.is_none() && !(issue_default(launch) && has_default(&settings).await?) {
        launch.agent = agents::pick_default_agent(ctx, settings.get().await?)?;
    }
    Ok(Some(settings))
}

/// Whether `launch` names nothing a new workspace starts from: no branch,
/// issue, link, base or copy.
fn names_nothing(launch: &Launch) -> bool {
    launch.branch.is_none()
        && launch.issue.is_none()
        && launch.base.is_none()
        && launch.copy.is_none()
        && launch.checkpoint.is_none()
        && launch.target.is_none()
}

/// `launch` starting from what a picker chose: the existing branch or PR
/// `creation` opens, or the issue `picked`.
fn picked_source(
    launch: &mut Launch,
    creation: Creation,
    picked: Option<crate::forge::items::reference::ItemInput>,
) {
    if let Some(issue) = picked {
        launch.issue = Some(IssueChoice::Named(issue));
    }
    match creation.pr {
        Some(crate::forge::items::reference::ItemInput::Url(pull)) => {
            launch.target = Some(StartTarget::Pull(pull));
        }
        _ => launch.branch = launch.branch.take().or(creation.existing),
    }
}

/// Whether the agent of `launch` defaults to `agent.default`, as a start on
/// a positional issue's does.
fn issue_default(launch: &Launch) -> bool {
    matches!(
        launch.target,
        Some(StartTarget::Issue(_) | StartTarget::Number(_))
    )
}

/// Whether `settings` name a default agent.
async fn has_default(settings: &AddSettings<'_>) -> Result<bool> {
    Ok(settings.get().await?.agent.default.is_some())
}

/// Send `launch`, which `agent` runs if it names one, and report what the
/// daemon started.
async fn send(ctx: &Context, launch: Launch, agent: Option<crate::agent::Agent>) -> Result<i32> {
    let launched = crate::client::launches::launch(&ctx.paths, launch);
    match launched.await? {
        Launched::Chat(started) => chat::report_started(ctx, started),
        Launched::Worker(worker) => {
            let QueuedWorker { workspace, entry } = *worker;
            ctx.show(&workspace, |_| ())?;
            match entry {
                Some(entry) => report_queued_in(ctx, &workspace, &entry),
                None => Ok(0),
            }
        }
        Launched::Workspace(started) => follow_workspace(ctx, *started, agent).await,
        Launched::Waiting { entry, waiting } => queued_agents::report_queued(ctx, &entry, waiting),
    }
}

/// Follow the preparation of the workspace the daemon `started`, which
/// queues `agent`'s launch once it is set up, and report the workspace and
/// the agent's entry.
async fn follow_workspace(
    ctx: &Context,
    started: StartedWorkspace,
    agent: Option<crate::agent::Agent>,
) -> Result<i32> {
    let preparing = crate::client::preparation::Preparing::follow(&ctx.paths, started).await?;
    // The preparation leaves the agent waiting should setup fail.
    let mut rest = AfterSetup {
        pr: None,
        agent: None,
        waiting: None,
    };
    let agent = ResolvedAddAgent {
        agent,
        model: Default::default(),
    };
    let finished = finish_add_workspace(ctx, preparing, &mut rest, &agent, false).await?;
    let Some(Prepared {
        workspace,
        agent: entry,
    }) = finished
    else {
        return Ok(1);
    };
    match entry {
        Some(entry) => report_queued_in(ctx, &workspace, &entry),
        None => Ok(0),
    }
}
