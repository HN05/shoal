//! `shoal start`: start an agent in a mode, which decides what the agent is
//! told, whether it starts in a repository and, unless the command says
//! otherwise, whether it starts in a workspace or a chat.
//!
//! Every start sends one launch, which the daemon records and queues, and
//! runs the agent of the space it recorded through one placement: here,
//! beside this terminal, in a background place or detached. A chat's agent
//! talks with the user about a repository in its registered checkout,
//! about a workspace's work in its worktree, or without a repository in
//! `root_dir`.
use anyhow::{Context as _, Result, ensure};
use shoal_ids::{agent_session_ids::AgentSessionId, space_ids::SpaceId};

use super::workspaces::{self, DaemonReads};
use crate::{
    agent::Agent,
    cli::{
        args::create::StartArgs,
        context::Context,
        ui::{self, Fallback, ModeChoice},
    },
    config::{
        Effective, modes,
        permissions::{PermissionChange, PermissionsConfig},
    },
    launches,
    model::{AgentSession, Repository, SpaceKind},
    protocol::ConfigTarget,
};
use shoal_protocol::launches::{IssueChoice, Launch, StartTarget};
use shoal_protocol::space_names::SpaceSelector;

/// Start what `args` ask for.
pub(super) async fn run(ctx: &Context, mut args: StartArgs) -> Result<i32> {
    ensure_scope_token(ctx)?;
    if let Some(parent) = &args.parent {
        args.parent = Some(SpaceSelector::Id(space_id(ctx, parent).await?));
    }
    let launch = args.launch();
    launches::decided_target(&launch)?;
    launches::ensure_copy_fits(&launch)?;
    ensure_nothing_dropped(&args)?;
    launches::ensure_in_fits(&launch)?;
    select_in_workspace(ctx, &mut args).await?;
    match start_of(ctx, &mut args).await? {
        Start::Workspace(mode) if args.in_workspace.is_some() => worker_in(ctx, args, mode).await,
        Start::Workspace(mode) => in_workspace(ctx, args, mode).await,
        Start::Chat(mode) => launch::chat(ctx, args, (mode, true)).await,
        Start::Outside(mode) => outside_repositories(ctx, args, mode).await,
    }
}

/// Refuse a workspace process without its scope token, which the daemon
/// would take for the user's: a start would create a space outside its own,
/// and a request would reach agents outside its scope.
pub(super) fn ensure_scope_token(ctx: &Context) -> Result<()> {
    ensure!(
        crate::env::is_scoped() || !crate::process::scoped::inherits_scope(&ctx.paths.state),
        "workspace processes start and ask agents only with their scope token"
    );
    Ok(())
}

/// The workspace `--in` names, else the current one or a picked one, by ID.
async fn select_in_workspace(ctx: &Context, args: &mut StartArgs) -> Result<()> {
    if let Some(workspace) = args.in_workspace.take() {
        let selected = ui::select_workspace(ctx, workspace, Fallback::CurrentDirectory).await?;
        args.in_workspace = Some(Some(selected));
    }
    Ok(())
}

/// Refuse the flags `args` sets that the kind of space `mode`, or the
/// other flags, start in cannot use, before anything is asked or created.
fn ensure_fits(args: &StartArgs, kind: SpaceKind, mode: Option<&modes::ModeName>) -> Result<()> {
    let unfit = args.terminal_workspace_flags();
    launches::ensure_fits(&args.launch(), &unfit, kind, mode)
}

/// The ID of the space `selector` names, by number or ID: one of this
/// machine, else one the hub lists on another joined machine.
async fn space_id(ctx: &Context, selector: &SpaceSelector) -> Result<SpaceId> {
    let space = crate::client::spaces::list(&ctx.paths)
        .await?
        .find(selector);
    if let Some(space) = space {
        return Ok(space.id);
    }
    let elsewhere = super::machines::space_elsewhere(ctx, selector).await?;
    Ok(elsewhere
        .with_context(|| format!("no space {selector}"))?
        .0
        .space
        .id)
}

/// Where a start goes, and in which mode.
enum Start {
    Workspace(ModeChoice),
    /// A chat in a repository.
    Chat(ModeChoice),
    /// A chat without a repository, in the mode named or picked here.
    Outside(modes::ModeName),
}

/// Where the start goes and its mode: the mode `--mode` names, else on a
/// terminal the plan mode for `--issue` without a value, which no other
/// mode takes, or one picked once the arguments given are checked, else
/// the [`default_mode`]. When the arguments decide the kind of space, its
/// own start picks among the modes of that kind in a repository after its
/// checks; otherwise the mode is the [`picked_mode`] here.
async fn start_of(ctx: &Context, args: &mut StartArgs) -> Result<Start> {
    let named = match &args.mode {
        Some(mode) => {
            modes::ensure_startable(mode)?;
            Some(mode.clone())
        }
        None if !ui::can_pick(ctx) => Some(launches::default_mode(&args.launch())),
        None if args.issue == Some(None) => Some(modes::ModeName::PLAN),
        None => None,
    };
    if let Some(mode) = named {
        return shaped(ctx, args, mode).await;
    }
    if let Some(kind) = fitting_kind(args) {
        ensure_fits(args, kind, None)?;
        return Ok(match kind {
            SpaceKind::Workspace => Start::Workspace(ModeChoice::Pick),
            SpaceKind::Chat => Start::Chat(ModeChoice::Pick),
        });
    }
    let mode = picked_mode(ctx, args).await?;
    shaped(ctx, args, mode).await
}

/// Where a start in `mode` goes, by its [`shape`], refusing the flags
/// that kind of space cannot use.
async fn shaped(ctx: &Context, args: &StartArgs, mode: modes::ModeName) -> Result<Start> {
    let (kind, repository) = shape(ctx, args, &mode).await?;
    ensure_fits(args, kind, Some(&mode))?;
    Ok(match (kind, repository) {
        (SpaceKind::Workspace, _) => Start::Workspace(ModeChoice::Named(mode)),
        (SpaceKind::Chat, true) => Start::Chat(ModeChoice::Named(mode)),
        (SpaceKind::Chat, false) => Start::Outside(mode),
    })
}

/// The mode the labels of the issue the arguments name call for in the
/// repository `selector` names, by [`labelled_mode`]; none without such an
/// issue or a label to look for.
async fn issue_mode(
    ctx: &Context,
    args: &StartArgs,
    selector: crate::repository::Selector,
    settings: &Effective,
) -> Result<Option<modes::ModeName>> {
    let Some(input) = launches::named_issue(&args.launch())? else {
        return Ok(None);
    };
    if settings.issue.plan_label.is_none() && settings.issue.planned_label.is_none() {
        return Ok(None);
    }
    let repos = crate::client::repositories::list(&ctx.paths).await?;
    let repo = crate::repository::select(&repos, selector).await?;
    let context = super::issues::read_context(ctx, repo).await?;
    let issue = super::issues::load(ctx, context, input).await?;
    let mode = launches::labelled_mode(&settings.issue, &issue.labels);
    Ok(mode.filter(|mode| modes::get(&settings.modes, mode).is_ok()))
}

/// The kind of space the arguments other than the mode fit: a workspace
/// for a flag only a workspace takes or a PR or branch link.
fn fitting_kind(args: &StartArgs) -> Option<SpaceKind> {
    let terminal = !args.terminal_workspace_flags().is_empty();
    launches::fitting_kind(&args.launch()).or(terminal.then_some(SpaceKind::Workspace))
}

/// The flags `args` sets that only a workspace takes.
fn workspace_flags(args: &StartArgs) -> Vec<&'static str> {
    [
        launches::workspace_flags(&args.launch()),
        args.terminal_workspace_flags(),
    ]
    .concat()
}

/// The mode picked for a start whose arguments fit either kind of space,
/// once the agent and model, and any repository or issue it names with its
/// parent, are checked: the [`issue_mode`] of an issue it names, else one
/// picked among the modes in a repository when it names one, else among
/// them all, by the [`mode_settings`].
async fn picked_mode(ctx: &Context, args: &mut StartArgs) -> Result<modes::ModeName> {
    let selector = picking_repository(ctx, args).await?;
    let settings = match (&selector, &args.in_workspace) {
        (Some(selector), None) => {
            let target = ConfigTarget::Repository(selector.clone());
            shoal_workers::settings::of(&ctx.paths, target).await?
        }
        _ => mode_settings(ctx, args).await?,
    };
    // Another machine checks its own agents.
    if args.machine.is_none() {
        let model = args.model.clone().into();
        crate::agents::checked_agent(&settings, args.agent.clone(), &model)?;
    }
    let fit = ui::ModeFit {
        kind: None,
        repository: selector.is_some().then_some(true),
    };
    if let Some(selector) = selector
        && let Some(mode) = issue_mode(ctx, args, selector, &settings).await?
    {
        return Ok(mode);
    }
    ModeChoice::Pick.chosen(ctx, &settings.modes, fit)
}

/// The repository a start whose mode is picked goes in, if the arguments
/// name one: that of the existing workspace `--in` names, whose worktree
/// the start is in whatever its mode, else the [`checked_repository`] they
/// name.
async fn picking_repository(
    ctx: &Context,
    args: &mut StartArgs,
) -> Result<Option<crate::repository::Selector>> {
    if let Some(Some(workspace)) = &args.in_workspace {
        let inspection = crate::client::workspaces::inspect(&ctx.paths, workspace.clone()).await?;
        return Ok(Some((inspection.workspace.repository()).into()));
    }
    let names_repository =
        args.repository.is_some() || args.repository_override.is_some() || args.issue.is_some();
    Ok(match names_repository {
        true => Some(checked_repository(ctx, args).await?),
        false => None,
    })
}

/// The repository the start goes in, resolved and checked with its parent
/// as a workspace's start does. It is then named where the positional
/// argument named it, else with `--repo`, so neither kind of start asks
/// for it again.
async fn checked_repository(
    ctx: &Context,
    args: &mut StartArgs,
) -> Result<crate::repository::Selector> {
    let (repository, issue) = launches::chat_target(
        args.repository.clone(),
        args.repository_override.clone(),
        args.issue.clone().map(IssueChoice::from),
        false,
    )?;
    let issue = issue.as_ref().and_then(IssueChoice::named);
    let parent = args.parent.as_ref();
    let named = repository
        .clone()
        .map(ui::repository_selector)
        .transpose()?;
    let selector = workspaces::check_target(ctx, named, issue, parent).await?;
    let named = selector.value().to_owned();
    match matches!(args.repository, Some(StartTarget::Repository(_))) {
        true => args.repository = Some(StartTarget::Repository(named)),
        false => args.repository_override = Some(named),
    }
    args.checked_repository = Some(selector.clone());
    Ok(selector)
}

/// The kind of space the start creates and whether `mode` starts it in a
/// repository, as [`launches::shape::launch_kind`] decides it for the
/// daemon too.
async fn shape(
    ctx: &Context,
    args: &StartArgs,
    mode: &modes::ModeName,
) -> Result<(SpaceKind, bool)> {
    launches::shape::launch_kind(&DaemonReads(ctx), &located(args)?, mode).await
}

/// The settings whose modes decide the kind, as
/// [`launches::shape::launch_settings`] finds them. Nothing is asked.
async fn mode_settings(ctx: &Context, args: &StartArgs) -> Result<Effective> {
    launches::shape::launch_settings(&DaemonReads(ctx), &located(args)?).await
}

/// The registered repository `args` name, or the one their item URL, the
/// space they go below or the current checkout or workspace belongs to,
/// without asking.
async fn mode_repository(ctx: &Context, args: &StartArgs) -> Result<Option<Repository>> {
    launches::shape::launch_repository(&DaemonReads(ctx), &located(args)?).await
}

/// The launch `args` ask for, made in this process's directory.
fn located(args: &StartArgs) -> Result<Launch> {
    Ok(Launch {
        directory: ui::current_directory()?,
        ..args.launch()
    })
}

/// Start the agent in a new or reopened workspace, or, for a start that
/// names no branch, issue, PR or copy, in a chat that its
/// agent makes a workspace once it knows the work.
pub(super) async fn in_workspace(ctx: &Context, args: StartArgs, mode: ModeChoice) -> Result<i32> {
    let unsettled = args.clone();
    match launch::workspace(ctx, args, mode).await {
        Err(error) if error.is::<workspaces::StartsAsChat>() => {
            let starts = error.downcast::<workspaces::StartsAsChat>()?;
            as_chat(ctx, unsettled, starts).await
        }
        started => started,
    }
}

/// Start the agent of the workspace's start `args`, which named no branch,
/// as a start or a contribution may not, in a chat in the repository
/// `starts` names, in the mode it chose, the worker's by default, refusing
/// what only a new workspace takes.
async fn as_chat(
    ctx: &Context,
    mut args: StartArgs,
    starts: workspaces::StartsAsChat,
) -> Result<i32> {
    if let Some(flag) = workspace_flags(&args).first() {
        anyhow::bail!(
            "{flag} applies to a new workspace's branch; name the branch with --branch, since a start without one begins as a chat"
        );
    }
    args.repository = None;
    args.repository_override = Some(starts.repository.value().to_owned());
    args.checked_repository = None;
    let mode = starts.mode.unwrap_or(modes::ModeName::DEFAULT);
    launch::chat(ctx, args, (ModeChoice::Named(mode), true)).await
}

/// Start the worker of the existing workspace `--in` names, in `mode`, with
/// the arguments after `--`, else on the workspace's issue.
async fn worker_in(ctx: &Context, args: StartArgs, mode: ModeChoice) -> Result<i32> {
    let workspace = (args.in_workspace.flatten()).context("--in names no workspace")?;
    let (agent, fork) = match args.fork {
        true => {
            let (_, agent, session) = forked(ctx, None, args.agent).await?;
            (Some(agent), Some(session))
        }
        false => (args.agent, None),
    };
    let options = workspaces::AddOptions {
        here: args.here,
        focus: args.focus,
        wait: args.wait,
        now: args.now,
        blockers: args.add_blocker,
        model: args.model.into(),
        mode: Some(mode),
        narrowing: given(&args.permissions),
        fork,
    };
    workspaces::start_worker(ctx, workspace, agent, args.args, options).await
}

/// Refuse a `--permission KEY=mode`, which drops a value given before, and
/// a new agent has none.
fn ensure_nothing_dropped(args: &StartArgs) -> Result<()> {
    ensure!(
        PermissionChange::combined(&args.permissions)
            .dropped
            .is_empty(),
        "--permission KEY={} drops a value given before, and a new agent has none; leave the key out",
        crate::config::permissions::MODE_VALUE
    );
    Ok(())
}

/// The parent, agent and session of a `--fork` start: one forked from the
/// `copied` workspace's agent goes below `parent` as any start does, while
/// one forked from the parent's agent goes below that space.
async fn fork_from(
    ctx: &Context,
    copied: Option<SpaceSelector>,
    parent: Option<SpaceSelector>,
    agent: Option<Agent>,
) -> Result<(Option<SpaceSelector>, Option<Agent>, Option<AgentSessionId>)> {
    if copied.is_some() {
        let (_, agent, session) = forked(ctx, copied, agent).await?;
        return Ok((parent, Some(agent), Some(session)));
    }
    let (parent, agent, session) = forked(ctx, parent, agent).await?;
    Ok((Some(parent), Some(agent), Some(session)))
}

/// The space, a workspace or a chat, whose agent `--fork` forks, `space`
/// or else the current one, the agent that starts from the fork, `agent` or
/// else that one, and the session it forks. An agent that cannot start from
/// a fork is refused before anything is created.
async fn forked(
    ctx: &Context,
    space: Option<SpaceSelector>,
    agent: Option<Agent>,
) -> Result<(SpaceSelector, Agent, AgentSessionId)> {
    let parent = ui::select_space(ctx, space, Fallback::CurrentDirectory).await?;
    let AgentSession {
        agent: running,
        session,
        ..
    } = crate::client::agents::session(&ctx.paths, parent.clone()).await?;
    let agent = match agent {
        Some(agent) => agent,
        None => running.name().parse()?,
    };
    let target = ConfigTarget::Space(parent.clone());
    let settings = shoal_workers::settings::of(&ctx.paths, target).await?;
    crate::agents::ensure_forks(&settings, &agent)?;
    Ok((parent, agent, session))
}

/// Start the agent in a chat without a repository, in `root_dir`; it starts
/// on no repository or issue.
async fn outside_repositories(
    ctx: &Context,
    args: StartArgs,
    mode: modes::ModeName,
) -> Result<i32> {
    launches::ensure_no_repository(&args.launch(), &mode)?;
    launch::chat(ctx, args, (ModeChoice::Named(mode), false)).await
}

/// The permissions a start's `--permission` assignments give its agent,
/// among which [`ensure_nothing_dropped`] refused a `KEY=mode`.
fn given(changes: &[PermissionChange]) -> PermissionsConfig {
    PermissionChange::combined(changes).set
}

mod agent;
mod attach;
mod detached;
mod issue;
mod launch;
pub(in crate::cli::commands) mod placement;
mod reports;
mod run;

pub(super) use agent::check_model;
pub(in crate::cli) use attach::execute_place;
pub(in crate::cli::commands) use attach::{Placed, place, run as run_attached, send};
pub(in crate::cli::commands) use detached::{ensure_detachable, leaves_caller, place_agent};
pub(in crate::cli::commands) use placement::Attach;
pub(in crate::cli::commands) use reports::report_queued_in;
pub(in crate::cli) use run::run_entry;

#[cfg(test)]
mod tests;
