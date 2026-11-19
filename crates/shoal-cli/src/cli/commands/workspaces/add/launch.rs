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
    finish::finish_add_workspace,
    mode::choose_add_mode,
    plan::launch_args,
    prepared,
    resolve::{Resolved, resolve},
    source::pick_add_source,
    target::resolve_add_target,
};
use crate::{
    cli::{
        agents,
        commands::{
            links, queued_agents,
            start::{self, Attach, placement},
        },
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
use shoal_protocol::launches::{IssueChoice, Launch, SpaceChoice, StartTarget};
use shoal_protocol::protocol::{Launched, Started};
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
        Some(_) => placement::in_caller(ctx, (&mut launch, issue_linked), settings, &options),
        None => start::ensure_detachable(settings, launch.agent.as_ref(), &options)?,
    }
    start::send(ctx, launch, (false, None), attach.zip(Some(settings))).await
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
    // Another machine starts its agent there.
    let leaves = start::leaves_caller(ctx, options.here) || launch.machine.is_some();
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
    start::place_agent(ctx, (&mut launch, issue_linked), placed, settings, options)?;
    let attach = attach.zip(Some(settings));
    start::send(ctx, launch, (options.wait, agent), attach).await
}

/// The chat a start in this caller begins as, since `launch` names nothing
/// its workspace starts from: in the repository it names, else that of the
/// space it goes below, else the current one.
async fn starts_as_chat(ctx: &Context, launch: &Launch) -> Result<StartsAsChat> {
    let named = (launch.repository.clone()).or_else(|| match &launch.target {
        Some(StartTarget::Repository(named)) => Some(named.clone()),
        _ => None,
    });
    let named = named.map(crate::repository::named).transpose()?;
    let parent = launch.parent.as_ref();
    let repository = resolve_add_target(ctx, named, None, parent).await?.selector;
    Ok(StartsAsChat {
        repository,
        mode: launch.mode.clone(),
    })
}

/// The agent `launch` starts under `settings`: the one it names, else
/// `agent.default`.
fn starting_agent(launch: &Launch, settings: &Effective) -> Option<crate::agent::Agent> {
    (launch.agent.clone()).or_else(|| settings.agent.default.clone())
}

/// Pick on a terminal what the `resolved` start `launch` leaves to a
/// picker: what it starts from and its agent. A start that begins as a chat
/// picks its agent there.
async fn pick_missing(ctx: &Context, launch: &mut Launch, resolved: &Resolved<'_>) -> Result<()> {
    if names_nothing(launch) {
        let mut creation = Creation::default();
        let picked = pick_add_source(ctx, &resolved.target, &mut creation).await?;
        picked_source(launch, creation, picked);
    }
    if begins_as_chat(launch) {
        return Ok(());
    }
    let settings = resolved.settings.get().await?;
    launch.agent = agents::start_agent(ctx, settings, launch.agent.take())?;
    Ok(())
}

/// Enter the workspace a creation `started` without an agent where a start
/// without one enters it from the user's terminal: here, or in a place
/// beside it, its shell opened once its preparation ends. `None` where the
/// caller follows the preparation itself, as an agent's creation does.
pub(in crate::cli::commands) async fn enter_created(
    ctx: &Context,
    launch: &Launch,
    started: &StartedWorkspace,
) -> Result<Option<i32>> {
    // A new workspace's worktree may not exist yet, so its repository's
    // settings place it, as they place a start's.
    let workspace = &started.workspace;
    let settings = match started.reused {
        true => AddSettings::workspace(ctx, &workspace.id),
        false => AddSettings::new(ctx, &workspace.repository().into()),
    };
    let settings = settings.get().await?;
    let options = AddOptions::default();
    let Some(attach) = Attach::of(ctx, (launch, false), None, settings, &options) else {
        return Ok(None);
    };
    links::print_warnings(&started.warnings);
    let placed = start::Placed {
        space: Box::new((*started.workspace).clone().into()),
        operation: started.operation.clone(),
        entry: started.entry.clone(),
    };
    let code = start::run_attached(ctx, placed, (attach, settings)).await?;
    Ok(Some(code))
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

/// Report the workspace the daemon `launched`, whose agent, which `agent`
/// runs if it names one, runs in no terminal of this caller: once it is
/// recorded, or with `wait` once its preparation ends, unless the
/// preparation waits for other work or disk space, which leaves nothing to
/// follow yet.
pub(in crate::cli::commands) async fn report_launched(
    ctx: &Context,
    launched: Launched,
    (wait, agent): (bool, Option<crate::agent::Agent>),
) -> Result<i32> {
    let worker = matches!(launched.start, Started::Worker);
    let follows = wait
        && matches!(
            launched.start,
            Started::Preparing {
                operation: Some(_),
                ..
            }
        );
    let started = prepared::workspace_start(launched)?;
    match (worker, follows) {
        (true, _) => report_worker(ctx, started),
        (false, true) => follow_workspace(ctx, started, agent).await,
        (false, false) => report_workspace(ctx, started),
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
    let agent = ResolvedAddAgent {
        agent,
        model: Default::default(),
    };
    let finished = finish_add_workspace(ctx, preparing, &agent, false).await?;
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
    start::report_queued_in(ctx, &workspace.name, &entry)
}

/// Report the existing workspace whose worker the daemon `started`, and
/// the entry of its agent, if it queued one.
fn report_worker(ctx: &Context, started: StartedWorkspace) -> Result<i32> {
    let StartedWorkspace {
        workspace, entry, ..
    } = started;
    ctx.show(&*workspace, |_| ())?;
    match entry {
        Some(entry) => start::report_queued_in(ctx, &workspace.name, &entry),
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
        Some(entry) => start::report_queued_in(ctx, &workspace.name, &entry),
        None => Ok(0),
    }
}

#[cfg(test)]
mod tests;
