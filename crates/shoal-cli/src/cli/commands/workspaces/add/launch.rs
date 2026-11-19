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
    plan::launch_args,
    resolve::{Resolved, resolve},
    source::pick_add_source,
    target::resolve_add_target,
    waiting::AfterSetup,
};
use crate::{
    cli::{
        agents,
        commands::{links, queued_agents, start},
        context::Context,
        output::{Palette, Style},
        ui::ModeChoice,
    },
    config::Effective,
    model::{QueueEntry, Workspace},
    protocol::operations::{Prepared, StartedWorkspace},
    start_plan::{Creation, ResolvedAddAgent},
    state::QueueState,
};
use shoal_protocol::launches::{IssueChoice, Launch, Launched, SpaceChoice, StartTarget, Started};
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
    let leaves = leaves_caller(ctx, options.here);
    // A start that runs in this caller settles what it starts on here, and
    // a terminal then picks what it leaves out.
    let resolved = match !leaves || ctx.interactive() {
        true => Some(resolve(ctx, &mut launch).await?),
        false => None,
    };
    launch.mode = match &resolved {
        Some(resolved) if ctx.interactive() => {
            pick_missing(ctx, &mut launch, resolved).await?;
            choose_add_mode(ctx, &resolved.settings, Some(mode)).await?
        }
        _ => mode.named(),
    };
    let (settings, issue_linked) = match &resolved {
        Some(resolved) => (resolved.settings.get().await?, resolved.issue_linked),
        None => (settings, false),
    };
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
    let launched = crate::client::launches::launch(&ctx.paths, launch).await?;
    match &launched.start {
        Started::Preparing {
            operation: None, ..
        } => report_workspace(ctx, attach::workspace_start(launched)?),
        Started::Preparing { .. } | Started::Worker => {
            Box::pin(attach::run(ctx, attach::workspace_start(launched)?)).await
        }
        Started::Queued { .. } | Started::Detached(_) => start::report_started(ctx, launched),
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

/// Pick on a terminal what the `resolved` start `launch` leaves to a
/// picker: what it starts from and its agent. A start that begins as a chat
/// picks its agent there.
async fn pick_missing(ctx: &Context, launch: &mut Launch, resolved: &Resolved<'_>) -> Result<()> {
    let mut picked_issue = false;
    if names_nothing(launch) {
        let mut creation = Creation::default();
        let picked = pick_add_source(ctx, &resolved.target, &mut creation).await?;
        picked_issue = picked.is_some();
        picked_source(launch, creation, picked);
    }
    if begins_as_chat(launch) {
        return Ok(());
    }
    let settings = &resolved.settings;
    // A picked issue takes `agent.default`, as a positional one does.
    if launch.agent.is_none() && picked_issue {
        launch.agent = settings.get().await?.agent.default.clone();
    }
    if launch.agent.is_none() && !(issue_default(launch) && has_default(settings).await?) {
        launch.agent = agents::pick_default_agent(ctx, settings.get().await?)?;
        if launch.agent.is_none() {
            without_agent(launch)?;
        }
    }
    Ok(())
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

/// Settle on a terminal what the new workspace `launch` starts in and
/// from, as a start's pickers do: its repository, picked when nothing names
/// one, and, when it names nothing its branch comes from, its source;
/// without a terminal, refuse a launch that names no source.
pub(in crate::cli::commands) async fn pick_start_source(
    ctx: &Context,
    launch: &mut Launch,
) -> Result<()> {
    // A number beside a branch or issue of the start's own names a
    // repository, which the pickers then start in.
    launch.target = crate::launches::decided_target(launch)?;
    if !ctx.interactive() {
        anyhow::ensure!(
            !begins_as_chat(launch),
            "name the workspace's branch with --branch, --issue or an issue, PR or branch link"
        );
        return Ok(());
    }
    let resolved = resolve(ctx, launch).await?;
    if !begins_as_chat(launch) {
        return Ok(());
    }
    let mut creation = Creation::default();
    let picked = pick_add_source(ctx, &resolved.target, &mut creation).await?;
    picked_source(launch, creation, picked);
    // A new branch the picker chose still needs its name.
    if begins_as_chat(launch) {
        let check = async |name: String| shoal_core::git::check_branch_name(&name).map(|()| name);
        launch.branch = Some(crate::cli::ui::input_until(ctx, "Branch name", None, check).await?);
    }
    Ok(())
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
pub(super) fn issue_default(launch: &Launch) -> bool {
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
    match &launched.start {
        Started::Queued { .. } | Started::Detached(_) => start::report_started(ctx, launched),
        Started::Worker => report_worker(ctx, attach::workspace_start(launched)?),
        Started::Preparing {
            operation: Some(_), ..
        } if wait => follow_workspace(ctx, attach::workspace_start(launched)?, agent).await,
        Started::Preparing { .. } => report_workspace(ctx, attach::workspace_start(launched)?),
    }
}

/// Run here the agent of the space the daemon `launched` for this caller,
/// in this terminal or a focused place as `attach` says under `settings`.
async fn attached(
    ctx: &Context,
    launched: Launched,
    (attach, settings): (Attach, &Effective),
) -> Result<i32> {
    let started = match &launched.start {
        Started::Queued { .. } | Started::Detached(_) => {
            return start::report_started(ctx, launched);
        }
        Started::Worker => attach::workspace_start(launched)?,
        // A preparation that waits for other work or disk space leaves the
        // agent to the queue's start once it is done.
        Started::Preparing {
            operation: None, ..
        } => return report_workspace(ctx, attach::workspace_start(launched)?),
        Started::Preparing { .. } => {
            let started = attach::workspace_start(launched)?;
            links::print_warnings(&started.warnings);
            started
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

/// Report the existing workspace whose worker the daemon `started`, and
/// the entry of its agent, if it queued one.
fn report_worker(ctx: &Context, started: StartedWorkspace) -> Result<i32> {
    let StartedWorkspace {
        workspace, entry, ..
    } = started;
    ctx.show(&*workspace, |_| ())?;
    match entry {
        Some(entry) => report_queued_in(ctx, &workspace, &entry),
        None => Ok(0),
    }
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
