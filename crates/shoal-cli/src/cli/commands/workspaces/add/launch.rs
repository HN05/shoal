//! A start the daemon carries out, whoever makes it: once this process has
//! picked what a terminal picks, it sends the start as one [`Launch`], and
//! either reports what the daemon started, leaving a workspace's
//! preparation and its agent's start to the daemon, or, for an agent that
//! runs in this caller, attaches it here.
use std::ffi::OsString;

use anyhow::Result;

use super::{
    AddOptions, StartsAsChat,
    agent::AddSettings,
    attach::{self, Attach},
    detached::{ensure_detachable, leaves_caller, unplaceable},
    finish::{finish_add_workspace, report_queued_in},
    mode::choose_add_mode,
    plan::{check_add_creation, launch_args},
    reads::DaemonReads,
    source::pick_add_source,
    target::{AddTarget, resolve_add_target},
    waiting::AfterSetup,
};
use crate::{
    cli::{
        agents,
        commands::{issues, links, queued_agents, start},
        context::Context,
        output::{Palette, Style},
        ui::ModeChoice,
    },
    config::Effective,
    model::{Blocker, QueueEntry, Workspace},
    protocol::operations::{Prepared, StartedWorkspace},
    start_plan::{Creation, ResolvedAddAgent},
    state::QueueState,
};
use shoal_protocol::launches::{
    IssueChoice, Launch, Launched, QueuedWorker, SpaceChoice, StartTarget,
};
use shoal_protocol::space_names::SpaceSelector;

/// Have the daemon start `agent` as the worker of `workspace`, in `mode`,
/// as `options` ask, with `args` as its task, and report it, or run it here
/// where it runs in this caller.
pub(super) async fn launch_worker(
    ctx: &Context,
    workspace: &Workspace,
    (mode, agent): (Option<crate::config::modes::ModeName>, ResolvedAddAgent),
    args: Vec<OsString>,
    (options, settings): (AddOptions, &AddSettings<'_>),
) -> Result<i32> {
    let settings = settings.get().await?;
    let mut launch = Launch {
        in_workspace: Some(SpaceChoice::Space(SpaceSelector::id(&workspace.id))),
        mode,
        agent: agent.agent,
        model: agent.model,
        fork: options.fork.clone(),
        permissions: options.narrowing,
        blockers: options.blockers.clone(),
        now: options.now,
        task: launch_args(args)?,
        env: Some(crate::terminal::start_environment()),
        ..Default::default()
    };
    // The worker starts on the workspace's issue unless told a task.
    let issue_linked = workspace.links.issue().is_some();
    let started = (&launch, issue_linked);
    let attach = Attach::of(ctx, started, launch.agent.as_ref(), settings, &options);
    match attach {
        Some(_) => attach::in_caller(ctx, (&mut launch, issue_linked), settings, &options),
        None => ensure_detachable(settings, launch.agent.as_ref(), &options)?,
    }
    send(ctx, launch, (false, None), attach.zip(Some(settings))).await
}

/// Have the daemon start `launch` in a new workspace, in the mode `mode`
/// names or, on a terminal, picks, once a terminal has picked what the
/// launch leaves to a picker, and run its agent here where it runs in this
/// caller. A start on a terminal, or whose agent runs in this caller, that
/// names no branch, issue, PR or copy begins as a chat instead. The
/// repository's `settings`, or the global config's without one, refuse an
/// agent they cannot start detached when no terminal picks the rest.
pub(in crate::cli::commands) async fn launch_workspace(
    ctx: &Context,
    mut launch: Launch,
    mode: ModeChoice,
    (options, settings): (&AddOptions, &Effective),
) -> Result<i32> {
    // A number beside a branch or issue of the start's own names a
    // repository, which the pickers then start in.
    launch.target = crate::launches::decided_target(&launch)?;
    let picked = pick_missing(ctx, &mut launch).await?;
    launch.mode = match &picked {
        Some(picked) => choose_add_mode(ctx, &picked.settings, Some(mode)).await?,
        None => mode.named(),
    };
    let (settings, issue_linked) = match &picked {
        Some(picked) => (picked.settings.get().await?, picked.issue_linked),
        None => (settings, false),
    };
    let leaves = leaves_caller(ctx, options.here);
    let agent = starting_agent(&launch, settings);
    let started = (&launch, issue_linked);
    let attach = Attach::of(ctx, started, agent.as_ref(), settings, options);
    if (!leaves || attach.is_some()) && begins_as_chat(&launch) {
        return Err(starts_as_chat(ctx, &launch).await?.into());
    }
    let placed = (attach, agent.as_ref());
    place_agent(ctx, (&mut launch, issue_linked), placed, settings, options)?;
    send(
        ctx,
        launch,
        (options.wait, agent),
        attach.zip(Some(settings)),
    )
    .await
}

/// Ready `launch` for where its `agent` runs: in this caller as `attach`
/// says, which needs the agent here before anything is created, or else
/// from the daemon, which refuses one no host can place for a start that
/// leaves this caller, as `--focus` is refused there.
fn place_agent(
    ctx: &Context,
    (launch, issue_linked): (&mut Launch, bool),
    (attach, agent): (Option<Attach>, Option<&crate::agent::Agent>),
    settings: &Effective,
    options: &AddOptions,
) -> Result<()> {
    if leaves_caller(ctx, options.here) {
        let refused = unplaceable(settings, agent, options)?;
        launch.unplaceable = refused.filter(|_| attach.is_none());
    }
    if attach.is_some() {
        if let Some(agent) = agent {
            crate::agents::ensure_available(agent, settings)?;
        }
        attach::in_caller(ctx, (launch, issue_linked), settings, options);
    }
    Ok(())
}

/// Have the daemon open the workspace `launch` names without starting an
/// agent, and follow its preparation here, where the caller's shell enters
/// it.
pub(in crate::cli::commands) async fn open_here(ctx: &Context, mut launch: Launch) -> Result<i32> {
    launch.here = true;
    match crate::client::launches::launch(&ctx.paths, launch).await? {
        Launched::Workspace(started) if started.operation.is_some() => {
            Box::pin(attach::run(ctx, *started)).await
        }
        Launched::Workspace(started) => report_workspace(ctx, *started),
        Launched::Worker(worker) => Box::pin(attach::run(ctx, attach::of_worker(*worker))).await,
        Launched::Chat(started) => start::report_started(ctx, started),
    }
}

/// The chat a start in this caller begins as, since `launch` names nothing
/// its workspace starts from: in the repository it names, else the
/// current one.
async fn starts_as_chat(ctx: &Context, launch: &Launch) -> Result<StartsAsChat> {
    let named = (launch.repository.clone()).or_else(|| match &launch.target {
        Some(StartTarget::Repository(named)) => Some(named.clone()),
        _ => None,
    });
    let named = named.map(crate::repository::named).transpose()?;
    let repository = resolve_add_target(ctx, named, None).await?.selector;
    Ok(StartsAsChat {
        repository,
        mode: launch.mode.clone(),
    })
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
/// What a terminal's pickers settled a start under: the settings of the
/// repository, or of the workspace whose branch it names, and whether that
/// workspace's own issue prompts its agent.
struct Picked<'a> {
    settings: AddSettings<'a>,
    issue_linked: bool,
}

async fn pick_missing<'a>(ctx: &'a Context, launch: &mut Launch) -> Result<Option<Picked<'a>>> {
    if !ctx.interactive() {
        return Ok(None);
    }
    let named = (launch.repository.clone()).or_else(|| match &launch.target {
        Some(StartTarget::Repository(named)) => Some(named.clone()),
        _ => None,
    });
    let issue = launch.issue.as_ref().and_then(IssueChoice::named).cloned();
    let named = match named {
        Some(named) => Some(crate::repository::named(named)?),
        None => linked_repository(ctx, launch).await?,
    };
    let target = resolve_add_target(ctx, named, issue.as_ref()).await?;
    launch.repository = Some(target.repository(ctx).await?.id.to_string());
    if matches!(launch.target, Some(StartTarget::Repository(_))) {
        launch.target = None;
    }
    let selector = Some(target.selector.clone());
    crate::cli::commands::permissions::ensure_held(ctx, selector, &launch.permissions).await?;
    check_named_blockers(ctx, &target, &launch.blockers).await?;
    // A branch a workspace has reopens it, under its own settings.
    let reopened = check_launch_creation(ctx, &target, launch).await?;
    let issue_linked =
        (reopened.as_ref()).is_some_and(|workspace| workspace.links.issue().is_some());
    let settings = match &reopened {
        Some(workspace) => AddSettings::workspace(ctx, &workspace.id),
        None => AddSettings::new(ctx, &target.selector),
    };
    ensure_agent_runs(&settings, launch).await?;
    if names_nothing(launch) {
        let mut creation = Creation::default();
        let picked = pick_add_source(ctx, &target, &mut creation).await?;
        picked_source(launch, creation, picked);
    }
    // An issue that cannot be read ends the start before a picker asks more.
    if let Some(issue) = named_issue(launch)? {
        let context = issues::read_context(ctx, target.repository(ctx).await?).await?;
        issues::load(ctx, context, issue).await?;
    }
    let picked = |settings| Picked {
        settings,
        issue_linked,
    };
    // A start that begins as a chat picks its agent there.
    if begins_as_chat(launch) {
        return Ok(Some(picked(settings)));
    }
    if launch.agent.is_none() && !(issue_default(launch) && has_default(&settings).await?) {
        launch.agent = agents::pick_default_agent(ctx, settings.get().await?)?;
        if launch.agent.is_none() {
            without_agent(launch)?;
        }
    }
    Ok(Some(picked(settings)))
}

/// The issue `launch` starts on, by `--issue` or the positional argument.
fn named_issue(launch: &Launch) -> Result<Option<crate::forge::items::reference::ItemInput>> {
    if let Some(issue) = launch.issue.as_ref().and_then(IssueChoice::named) {
        return Ok(Some(issue.clone()));
    }
    Ok(
        match (launch.target.clone())
            .map(|target| target.decided(false))
            .transpose()?
        {
            Some(StartTarget::Issue(issue)) if issue_default(launch) => Some(issue),
            _ => None,
        },
    )
}

/// `launch` starting no agent, as the picker chose: a positional issue,
/// whose start would take `agent.default`, names the issue as `--issue`
/// does instead, which starts none.
fn without_agent(launch: &mut Launch) -> Result<()> {
    if !issue_default(launch) {
        return Ok(());
    }
    if let Some(StartTarget::Issue(issue)) = (launch.target.take())
        .map(|target| target.decided(false))
        .transpose()?
    {
        launch.issue = Some(IssueChoice::Named(issue));
    }
    Ok(())
}

/// Refuse the workspaces, queued agents and issues a start `named` as its
/// blockers in `target`'s repository when the daemon knows none of them,
/// before a picker asks anything.
pub(super) async fn check_named_blockers(
    ctx: &Context,
    target: &AddTarget,
    named: &[String],
) -> Result<()> {
    if named.is_empty() {
        return Ok(());
    }
    let repo = target.repository(ctx).await?;
    let mut blockers = Vec::new();
    for input in named {
        blockers.push(match input.parse()? {
            Blocker::Issue(issue) => Blocker::Issue(issues::issue(ctx, repo, &issue).await?.into()),
            blocker => blocker,
        });
    }
    let selector = target.selector.clone();
    crate::client::workspaces::check_blockers(&ctx.paths, selector, blockers).await
}

/// Refuse the agent `launch` names, and the model it asks of it, before a
/// picker asks anything, when `settings` cannot start them.
async fn ensure_agent_runs(settings: &AddSettings<'_>, launch: &Launch) -> Result<()> {
    let Some(agent) = &launch.agent else {
        return Ok(());
    };
    let settings = settings.get().await?;
    crate::agents::ensure_available(agent, settings)?;
    let model = crate::agents::resolve_model(settings, agent, launch.model.clone());
    crate::agents::check_model(settings, agent, &model)
}

/// Refuse, before a picker asks anything, what creating the workspace
/// `launch` names in `target` would refuse, returning instead the
/// workspace that already has the branch it names, which the start opens.
async fn check_launch_creation(
    ctx: &Context,
    target: &AddTarget,
    launch: &Launch,
) -> Result<Option<Workspace>> {
    let mut creation = Creation {
        path: launch.path.clone(),
        branch: launch.branch.clone(),
        base: launch.base.clone(),
        git_profile: launch.git_profile.clone(),
        options: crate::protocol::CreationOptions {
            name: launch.name.clone(),
            priority: launch.priority,
            skip_setup: launch.skip_setup,
            disposable: launch.disposable,
        },
        ..Creation::default()
    };
    let repo = (&target.selector, target.repository(ctx).await?);
    let reads = DaemonReads(ctx);
    let settle = crate::launches::workspace::settle(&reads, repo, &mut creation);
    if let Some(workspace) = settle.await? {
        ensure_takes_existing(launch, &creation, &workspace)?;
        return Ok(Some(workspace));
    }
    check_add_creation(ctx, target, &creation, launch.parent.as_ref()).await?;
    Ok(None)
}

/// Refuse what only a new workspace takes for `launch`, which goes into the
/// existing `workspace` that has the branch it names, as the daemon refuses
/// it. The parent of a fork without a copy is the workspace whose agent it
/// forks, which an existing workspace's start takes too.
fn ensure_takes_existing(
    launch: &Launch,
    creation: &Creation,
    workspace: &Workspace,
) -> Result<()> {
    let forks_parent = launch.fork.is_some() && creation.copy.is_none();
    let parent = launch.parent.is_some() && !forks_parent;
    let issue = launch.issue.is_some();
    let Some(flag) = crate::launches::workspace::new_space_flag(creation, issue, parent) else {
        return Ok(());
    };
    let branch = workspace.branch().map_or("", |branch| branch.as_str());
    anyhow::bail!(
        "{flag} applies to a new workspace, and workspace {} already has branch {branch}; the start goes into it as --in does",
        workspace.number
    )
}

/// The registered repository the link `launch` starts on belongs to,
/// offering to register an unregistered one; none for a start on no link.
async fn linked_repository(
    ctx: &Context,
    launch: &Launch,
) -> Result<Option<crate::repository::Selector>> {
    let Some(linked) = (launch.target.as_ref()).and_then(StartTarget::linked_repository) else {
        return Ok(None);
    };
    let mut repos = crate::client::repositories::list(&ctx.paths).await?;
    let retry = "shoal start <link> --repo <repository>";
    let id = issues::registered_remote(ctx, &mut repos, linked.clone(), retry).await?;
    Ok(Some(id.into()))
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

/// Whether `launch` names nothing its workspace's branch comes from, no
/// branch, issue, link or copy, so it begins as a chat; a base or a
/// repository alone settles no branch.
fn begins_as_chat(launch: &Launch) -> bool {
    let names_link = !matches!(launch.target, None | Some(StartTarget::Repository(_)));
    launch.branch.is_none()
        && launch.issue.is_none()
        && launch.copy.is_none()
        && launch.checkpoint.is_none()
        && !names_link
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
        _ => {
            let picked = creation.existing.as_deref().map(switched_branch);
            launch.branch = launch.branch.take().or(picked);
        }
    }
}

/// The branch the picked ref `selector` names, as the daemon settles a
/// start's branch: a local branch by its name, and a remote's by its whole
/// ref, which keeps the remote it was picked from.
fn switched_branch(selector: &str) -> String {
    crate::git::strip_local(selector)
        .unwrap_or(selector)
        .to_owned()
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
/// daemon started: a new workspace once it is recorded, or with `wait` once
/// its preparation ends, unless the preparation waits for other work or
/// disk space, which leaves nothing to follow yet. An agent that runs in
/// this caller, as `attach` says under the start's settings, runs here once
/// its workspace is prepared and the queue lets it start.
async fn send(
    ctx: &Context,
    launch: Launch,
    (wait, agent): (bool, Option<crate::agent::Agent>),
    attach: Option<(Attach, &Effective)>,
) -> Result<i32> {
    let launched = crate::client::launches::launch(&ctx.paths, launch).await?;
    if let Some(attach) = attach {
        return attached(ctx, launched, attach).await;
    }
    match launched {
        Launched::Chat(started) => start::report_started(ctx, started),
        Launched::Worker(worker) => {
            let QueuedWorker { workspace, entry } = *worker;
            ctx.show(&workspace, |_| ())?;
            match entry {
                Some(entry) => report_queued_in(ctx, &workspace, &entry),
                None => Ok(0),
            }
        }
        Launched::Workspace(started) if wait && started.operation.is_some() => {
            follow_workspace(ctx, *started, agent).await
        }
        Launched::Workspace(started) => report_workspace(ctx, *started),
    }
}

/// Run here the agent of the space the daemon `launched` for this caller,
/// in this terminal or a focused place as `attach` says under `settings`.
async fn attached(
    ctx: &Context,
    launched: Launched,
    (attach, settings): (Attach, &Effective),
) -> Result<i32> {
    let started = match launched {
        Launched::Chat(started) => return start::report_started(ctx, started),
        Launched::Worker(worker) => attach::of_worker(*worker),
        // A preparation that waits for other work or disk space leaves the
        // agent to the queue's start once it is done.
        Launched::Workspace(started) if started.operation.is_none() => {
            return report_workspace(ctx, *started);
        }
        Launched::Workspace(started) => {
            links::print_warnings(&started.warnings);
            *started
        }
    };
    match attach {
        Attach::Here => Box::pin(attach::run(ctx, started)).await,
        Attach::Beside { focused } => {
            Box::pin(attach::run_beside(ctx, started, settings, focused)).await
        }
    }
}

/// Follow the preparation of the workspace the daemon `started`, and report
/// the workspace and the entry of its agent as the queue has it then.
async fn follow_workspace(
    ctx: &Context,
    started: StartedWorkspace,
    agent: Option<crate::agent::Agent>,
) -> Result<i32> {
    let entry = started.entry.clone();
    let preparing = crate::client::preparation::Preparing::follow(&ctx.paths, started).await?;
    // The daemon queued the agent, which waits for a failed setup itself.
    let mut rest = AfterSetup {
        pr: None,
        waiting: None,
    };
    let agent = ResolvedAddAgent {
        agent,
        model: Default::default(),
    };
    let finished = finish_add_workspace(ctx, preparing, &mut rest, &agent, false).await?;
    let Some(Prepared { workspace, .. }) = finished else {
        return Ok(1);
    };
    let Some(entry) = entry else {
        return Ok(0);
    };
    let entry = match queued_agents::current(ctx, &entry).await? {
        Some(entry) => entry,
        // It left the queue once its agent started.
        None => QueueEntry {
            state: QueueState::Starting,
            position: None,
            ..*entry
        },
    };
    report_queued_in(ctx, &workspace, &entry)
}

/// Report the workspace the daemon `started`, whose preparation goes on
/// without this process, and the entry of its agent, which waits for it.
pub(super) fn report_workspace(ctx: &Context, started: StartedWorkspace) -> Result<i32> {
    links::print_warnings(&started.warnings);
    let StartedWorkspace {
        workspace,
        reused,
        entry,
        ..
    } = started;
    ctx.show(&*workspace, |workspace| {
        outln!(
            "{} {} {}; `shoal status {}` shows its preparation",
            if reused { "Opened" } else { "Created" },
            Palette::stdout(ctx.json).paint(Style::Heading, &workspace.name),
            super::super::location_phrase(workspace),
            workspace.number,
        );
    })?;
    match entry {
        Some(entry) => report_queued_in(ctx, &workspace, &entry),
        None => Ok(0),
    }
}

#[cfg(test)]
mod tests;
