//! `shoal start`: start an agent in a mode, which decides what the agent is
//! told, whether it starts in a repository and, unless the command says
//! otherwise, whether it starts in a workspace or a chat.
use crate::client::repository_ids::RepositoryId;
use crate::client::{agent::AgentSessionId, space_ids::SpaceId};
use anyhow::{Context as _, Result, ensure};

use super::{
    chat,
    workspaces::{self, link::AddInput},
};
use crate::client::space_names::SpaceSelector;
use crate::{
    agent::Agent,
    cli::{
        args::{create::StartArgs, target::StartTarget},
        context::Context,
        ui::{self, Fallback, ModeChoice},
    },
    config::{
        self, Effective, modes,
        permissions::{PermissionChange, PermissionsConfig},
        templates,
    },
    forge::items::reference::{ItemInput, ItemRef},
    model::{AgentSession, SpaceKind},
    protocol::ConfigTarget,
    start_plan::{CopyCreation, Creation},
};

pub(super) async fn run(ctx: &Context, mut args: StartArgs) -> Result<i32> {
    let handoff = args.handoff.take();
    chat::handoff::serve(handoff.as_deref(), start(ctx, args)).await
}

/// Start what `args` ask for.
async fn start(ctx: &Context, mut args: StartArgs) -> Result<i32> {
    // Without its token, the daemon would take a workspace process's start
    // for the user's and create a space outside its own.
    ensure!(
        crate::env::is_scoped() || !crate::env::inherits_scope(&ctx.paths.state),
        "workspace processes start agents only with their scope token"
    );
    if let Some(parent) = &args.parent {
        args.parent = Some(SpaceSelector::Id(space_id(ctx, parent).await?));
    }
    ensure_copy_fits(&args)?;
    ensure_nothing_dropped(&args)?;
    ensure_in_fits(&args)?;
    select_in_workspace(ctx, &mut args).await?;
    match start_of(ctx, &mut args).await? {
        Start::Workspace(mode) if args.in_workspace.is_some() => worker_in(ctx, args, mode).await,
        Start::Workspace(mode) => in_workspace(ctx, args, mode).await,
        Start::Chat(mode) => in_chat(ctx, args, mode).await,
        Start::Outside(mode) => outside_repositories(ctx, args, mode).await,
    }
}

/// Refuse what a start in the existing workspace `--in` names takes in no
/// role, before its picker opens: a mode no start takes, a repository,
/// which is the workspace's, or what the worker mode, which always starts
/// the workspace's own agent, takes only for a new space.
fn ensure_in_fits(args: &StartArgs) -> Result<()> {
    if args.in_workspace.is_none() {
        return Ok(());
    }
    if let Some(mode) = &args.mode {
        modes::ensure_startable(mode)?;
    }
    if args.mode == Some(modes::ModeName::DEFAULT) && !asks_chat(args) {
        ensure_worker_fits(args)?;
    }
    let repository = args.repository_override.is_some()
        || matches!(args.repository, Some(StartTarget::Repository(_)));
    ensure!(
        !repository,
        "a start with --in starts in the workspace's repository; drop the repository"
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
    // A workspace takes every flag a chat does, but an existing one none
    // that names what a new space starts as.
    if kind == SpaceKind::Workspace {
        return ensure_worker_fits(args);
    }
    let unfit = workspace_flags(args);
    let hint = "start it in a mode that starts in a workspace, such as -m worker";
    let Some(flag) = unfit.first() else {
        return Ok(());
    };
    match mode {
        Some(mode) => {
            anyhow::bail!("{flag} does not apply to a {kind}, which mode {mode} starts in; {hint}")
        }
        None => anyhow::bail!("{flag} does not apply to a {kind}; {hint}"),
    }
}

/// Refuse what only a new space takes in a start of the worker of the
/// existing workspace `--in` names, picked or yet to be.
fn ensure_worker_fits(args: &StartArgs) -> Result<()> {
    let Some(workspace) = &args.in_workspace else {
        return Ok(());
    };
    let workspace =
        (workspace.as_ref()).map_or_else(|| "<workspace>".to_owned(), ToString::to_string);
    if let Some(Some(issue)) = &args.issue {
        anyhow::bail!(
            "--issue names a new space's issue; link it to the workspace with `shoal link issue {issue} assigned --space {workspace}`"
        );
    }
    let new_space = [
        ("<repository>", args.repository.is_some()),
        ("--repo", args.repository_override.is_some()),
        ("--issue", args.issue.is_some()),
        ("--parent", args.parent.is_some()),
        ("--name", args.name.is_some()),
        // `shoal priority` sets an existing workspace's.
        ("--priority", args.priority.is_some()),
    ];
    if let Some((flag, _)) = new_space.iter().find(|(_, set)| *set) {
        anyhow::bail!(
            "{flag} applies to a new space, and --in starts the worker of an existing workspace"
        );
    }
    Ok(())
}

/// The flags `args` sets that only a workspace takes.
fn workspace_flags(args: &StartArgs) -> Vec<&'static str> {
    [
        ("--branch", args.branch.is_some()),
        ("--path", args.path.is_some()),
        ("--base", args.base.is_some()),
        ("--git-profile", args.git_profile.is_some()),
        ("--fork", args.fork),
        ("--copy", args.copy.copy.is_some()),
        ("--checkpoint", args.copy.checkpoint.is_some()),
        ("--focus", args.focus),
        ("--now", args.now),
        ("--add-blocker", !args.add_blocker.is_empty()),
        ("--skip-setup", args.creation.skip_setup),
        ("--disposable", args.creation.disposable),
    ]
    .into_iter()
    .filter_map(|(flag, set)| set.then_some(flag))
    .collect()
}

/// The ID of the space `selector` names, by number or ID.
async fn space_id(ctx: &Context, selector: &SpaceSelector) -> Result<SpaceId> {
    let space = crate::client::workspaces::spaces(&ctx.paths)
        .await?
        .find(selector);
    Ok(space
        .with_context(|| format!("no space {selector}"))?
        .id()
        .clone())
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
        None if !ui::can_pick(ctx) => Some(default_mode(args)),
        None if args.issue == Some(None) => Some(modes::ModeName::PLAN),
        None => None,
    };
    if let Some(mode) = named {
        return shaped(ctx, args, mode).await;
    }
    if let Some(kind) = asked_kind(args).or_else(|| fitting_kind(args)) {
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
    let Some(input) = named_issue(args)? else {
        return Ok(None);
    };
    if settings.issue.plan_label.is_none() && settings.issue.planned_label.is_none() {
        return Ok(None);
    }
    let repos = crate::client::repositories::list(&ctx.paths).await?;
    let repo = crate::repository::select(&repos, selector).await?;
    let context = super::issues::read_context(ctx, repo).await?;
    let issue = super::issues::load(ctx, context, input).await?;
    let mode = labelled_mode(&settings.issue, &issue.labels);
    Ok(mode.filter(|mode| modes::get(&settings.modes, mode).is_ok()))
}

/// The mode for an issue labelled `labels`: the worker once `[issue]
/// planned_label` says its plan is settled, else the plan mode while
/// `plan_label` says it needs one.
fn labelled_mode(config: &config::Issue, labels: &[String]) -> Option<modes::ModeName> {
    let labelled =
        |label: &Option<String>| label.as_ref().is_some_and(|label| labels.contains(label));
    if labelled(&config.planned_label) {
        Some(modes::ModeName::DEFAULT)
    } else if labelled(&config.plan_label) {
        Some(modes::ModeName::PLAN)
    } else {
        None
    }
}

/// The issue `--issue` or the positional argument names by number or URL.
fn named_issue(args: &StartArgs) -> Result<Option<ItemInput>> {
    Ok(match (&args.issue, &args.repository) {
        (Some(Some(issue)), _) => Some(issue.clone()),
        (_, Some(positional)) => match positional.clone().decided(false)? {
            StartTarget::Issue(issue) => Some(issue),
            _ => None,
        },
        _ => None,
    })
}

/// Whether the positional argument links a PR or branch, which only a
/// workspace starts on.
fn links_branch(args: &StartArgs) -> bool {
    matches!(
        args.repository,
        Some(StartTarget::Pull(_) | StartTarget::Branch { .. })
    )
}

/// The kind of space the arguments other than the mode fit: a workspace
/// for a flag only a workspace takes or a PR or branch link.
fn fitting_kind(args: &StartArgs) -> Option<SpaceKind> {
    (links_branch(args) || !workspace_flags(args).is_empty()).then_some(SpaceKind::Workspace)
}

/// The mode picked for a start whose arguments fit either kind of space,
/// once the agent and model, and any repository or issue it names with its
/// parent, are checked: the [`issue_mode`] of an issue it names, else one
/// picked among the modes in a repository when it names one, else among
/// them all, by the current repository's settings or else the global
/// config's.
async fn picked_mode(ctx: &Context, args: &mut StartArgs) -> Result<modes::ModeName> {
    let selector = picking_repository(ctx, args).await?;
    let settings = match (&selector, &args.in_workspace) {
        (Some(selector), None) => {
            let target = ConfigTarget::Repository(selector.clone());
            crate::client::workspaces::settings(&ctx.paths, target).await?
        }
        _ => mode_settings(ctx, args).await?,
    };
    let model = args.model.clone().into();
    chat::checked_agent(&settings, args.agent.clone(), &model)?;
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
        return Ok(Some((&inspection.workspace.repository_id).into()));
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
    let (repository, issue) = chat_target(
        args.repository.clone(),
        args.repository_override.clone(),
        args.issue.clone(),
        false,
    )?;
    let issue = issue.flatten();
    let parent = args.parent.as_ref();
    let named = repository
        .clone()
        .map(ui::repository_selector)
        .transpose()?;
    let selector = workspaces::check_target(ctx, named, issue.as_ref(), parent).await?;
    let named = selector.value().to_owned();
    match matches!(args.repository, Some(StartTarget::Repository(_))) {
        true => args.repository = Some(StartTarget::Repository(named)),
        false => args.repository_override = Some(named),
    }
    args.checked_repository = Some(selector.clone());
    Ok(selector)
}

/// The mode without `--mode`: chat for a start that asks for a chat, whose
/// agent the worker mode would not tell it is one, else the worker.
fn default_mode(args: &StartArgs) -> modes::ModeName {
    match asks_chat(args) {
        true => modes::ModeName::CHAT,
        false => modes::ModeName::DEFAULT,
    }
}

/// The kind of space the start creates, the kind of `mode`, which must
/// exist before anything is created, unless the start [`asks_chat`], and
/// whether `mode` starts it in a repository. The default mode always exists
/// and always starts in a workspace, so it asks the daemon nothing; a
/// chat's start checks a mode found nowhere here itself once it has the
/// repository. With `--in`, the mode's kind decides whether the agent is the
/// workspace's worker or a chat in its worktree, which belongs to its
/// repository either way.
async fn shape(
    ctx: &Context,
    args: &StartArgs,
    mode: &modes::ModeName,
) -> Result<(SpaceKind, bool)> {
    if args.in_workspace.is_some() {
        return in_workspace_kind(ctx, args, mode)
            .await
            .map(|kind| (kind, true));
    }
    let asks_chat = asks_chat(args);
    if *mode == modes::ModeName::DEFAULT && !asks_chat {
        return Ok((SpaceKind::Workspace, true));
    }
    let settings = mode_settings(ctx, args).await?;
    let found = modes::get(&settings.modes, mode);
    if asks_chat {
        return Ok((SpaceKind::Chat, found.map_or(true, |mode| mode.repository)));
    }
    let found = found?;
    Ok((found.kind, found.repository))
}

/// The role of the agent `--in` starts in `mode`: the workspace's worker
/// for a mode that starts in a workspace, else a chat in its worktree, as a
/// start that [`asks_chat`] gets whatever the mode. A mode without a
/// repository is refused.
async fn in_workspace_kind(
    ctx: &Context,
    args: &StartArgs,
    mode: &modes::ModeName,
) -> Result<SpaceKind> {
    let settings = mode_settings(ctx, args).await?;
    let found = modes::get(&settings.modes, mode)?;
    ensure!(
        found.repository,
        "mode {mode} starts a chat without a repository, which --in cannot"
    );
    Ok(match asks_chat(args) {
        true => SpaceKind::Chat,
        false => found.kind,
    })
}

/// Whether the start continues a chat whatever its mode's kind: one the
/// place it opens in starts, or one a flag only a chat's start takes asks
/// for, such as a session it continues or a stopped chat it restores.
fn asks_chat(args: &StartArgs) -> bool {
    args.as_chat
        || args.session.is_some()
        || args.restore.is_some()
        || args.linked_issue.is_some()
        || args.handoff.is_some()
}

/// The kind of space a start that [`asks_chat`] asks for, whatever the
/// mode's kind.
fn asked_kind(args: &StartArgs) -> Option<SpaceKind> {
    asks_chat(args).then_some(SpaceKind::Chat)
}

/// The settings whose modes decide the kind: the workspace's `--in` names,
/// else the workspace that already has the branch `--branch` names, which
/// the start goes into as `--in` does, else the repository's
/// [`mode_repository`] finds, else the global config's. Nothing is asked.
async fn mode_settings(ctx: &Context, args: &StartArgs) -> Result<Effective> {
    if let Some(Some(workspace)) = &args.in_workspace {
        let target = ConfigTarget::Workspace(workspace.clone());
        return crate::client::workspaces::settings(&ctx.paths, target).await;
    }
    match mode_repository(ctx, args).await? {
        Some(id) => {
            let target = match branch_workspace(ctx, args, &id).await? {
                Some(workspace) => ConfigTarget::Workspace(SpaceSelector::Id(workspace)),
                None => ConfigTarget::Repository(id.into()),
            };
            crate::client::workspaces::settings(&ctx.paths, target).await
        }
        None => {
            crate::config::Config::load_with_templates(&ctx.paths)?.resolve(&Default::default())
        }
    }
}

/// The ID of the workspace of `repository` that already has the branch
/// `--branch` names, if any.
async fn branch_workspace(
    ctx: &Context,
    args: &StartArgs,
    repository: &RepositoryId,
) -> Result<Option<SpaceId>> {
    let Some(branch) = &args.branch else {
        return Ok(None);
    };
    let repos = crate::client::repositories::list(&ctx.paths).await?;
    let Some(repo) = repos.iter().find(|repo| repo.id == *repository) else {
        return Ok(None);
    };
    let owner = workspaces::owning_workspace(ctx, repo, branch).await?;
    Ok(owner.map(|workspace| workspace.id))
}

/// The ID of the registered repository `--repo` or the positional argument
/// names, else of the one an issue or item URL belongs to, else of the
/// current checkout or workspace, without asking.
async fn mode_repository(ctx: &Context, args: &StartArgs) -> Result<Option<RepositoryId>> {
    let repos = crate::client::repositories::list(&ctx.paths).await?;
    let named = (args.repository_override.as_deref()).or(positional_repository(args));
    if let Some(Ok(selector)) = named.map(|named| ui::repository_selector(named.to_owned()))
        && let Ok(repo) = crate::repository::select(&repos, selector).await
    {
        return Ok(Some(repo.id.clone()));
    }
    for item in linked_items(args) {
        if let Ok(Some(repo)) = super::issues::registered_for_item(&repos, item).await {
            return Ok(Some(repo.id.clone()));
        }
    }
    super::issues::current_repository_id(ctx, &repos).await
}

/// The repository the positional argument may name: a repository's, or a
/// number, which names a repository of that name until the start decides it
/// names an issue.
fn positional_repository(args: &StartArgs) -> Option<&str> {
    match &args.repository {
        Some(StartTarget::Repository(named) | StartTarget::Number(named)) => Some(named),
        _ => None,
    }
}

/// The issue and PR the arguments name by URL.
fn linked_items(args: &StartArgs) -> impl Iterator<Item = &ItemRef> {
    let issue = match &args.issue {
        Some(Some(ItemInput::Url(issue))) => Some(issue),
        _ => None,
    };
    let positional = match &args.repository {
        Some(StartTarget::Issue(ItemInput::Url(item)) | StartTarget::Pull(item)) => Some(item),
        _ => None,
    };
    issue.into_iter().chain(positional)
}

/// Start the agent in a new or reopened workspace, or, for a start that
/// names no branch, issue, PR or copy, in a chat that its
/// agent makes a workspace once it knows the work.
async fn in_workspace(ctx: &Context, args: StartArgs, mode: ModeChoice) -> Result<i32> {
    let unsettled = args.clone();
    match in_new_workspace(ctx, args, mode).await {
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
pub(super) async fn as_chat(
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
    let mut options = chat::Options::taken_from(&mut args);
    options.place = branchless_place(ctx.on_terminal(), options.place);
    in_chat_with(ctx, args, ModeChoice::Named(mode), options).await
}

/// Where the chat a branchless start begins as runs: beside this terminal
/// or here, as `place` says, on a terminal, and without one, which nothing
/// could run it in, in a background place.
fn branchless_place(on_terminal: bool, place: chat::ChatPlace) -> chat::ChatPlace {
    match (on_terminal, place) {
        (false, chat::ChatPlace::Beside) => chat::ChatPlace::Background,
        (_, place) => place,
    }
}

/// Start the agent in a new or reopened workspace.
async fn in_new_workspace(ctx: &Context, args: StartArgs, mode: ModeChoice) -> Result<i32> {
    let issue = match args.issue {
        Some(None) => anyhow::bail!("--issue needs a number or URL outside plan mode"),
        Some(Some(issue)) => Some(issue),
        None => None,
    };
    let copy = super::copies::source(ctx, args.copy).await?;
    let (parent, agent, fork) = match args.fork {
        true => {
            fork_from(
                ctx,
                copy.as_ref().map(|copy| copy.source.workspace.clone()),
                args.parent,
                args.agent,
            )
            .await?
        }
        false => (args.parent, args.agent, None),
    };
    // A copy goes in the copied workspace's repository unless one is named,
    // which the daemon refuses when it is another.
    let (positional, named) = named_repository(
        args.repository,
        args.repository_override,
        args.checked_repository,
    )?;
    let repository = match (&copy, &positional) {
        (Some(copy), None) => named.or_else(|| Some(copy.repository.clone().into())),
        _ => named,
    };
    let input = AddInput {
        positional,
        repository,
        issue,
    };
    let creation = Creation {
        path: args.path,
        // Settled once the repository is known: it may name a branch that
        // exists, or one a workspace has.
        branch: args.branch,
        existing: None,
        typed: false,
        base: args.base,
        git_profile: args.git_profile,
        pr: None,
        options: crate::protocol::CreationOptions {
            name: args.name,
            priority: args.priority,
            ..args.creation.into()
        },
        pull_head: None,
        copy: copy.map(|copy| CopyCreation {
            source: copy.source,
            name: copy.name,
        }),
    };
    let options = workspaces::AddOptions {
        here: args.here,
        focus: args.focus,
        now: args.now,
        blockers: args.add_blocker,
        model: args.model.into(),
        mode: Some(mode),
        narrowing: given(&args.permissions),
        prompt: None,
        parent,
        fork,
    };
    workspaces::link::add(ctx, input, creation, agent, args.args, options).await
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
        now: args.now,
        blockers: args.add_blocker,
        model: args.model.into(),
        mode: Some(mode),
        narrowing: given(&args.permissions),
        prompt: None,
        parent: None,
        fork,
    };
    workspaces::start_worker(ctx, workspace, agent, args.args, options).await
}

/// Refuse a copy beside a PR or branch link, which would open that branch
/// instead, before anything is asked or looked up.
fn ensure_copy_fits(args: &StartArgs) -> Result<()> {
    ensure!(
        !args.copy.is_set() || !links_branch(args),
        "a copy starts from the workspace it copies, not a PR or branch link"
    );
    Ok(())
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
/// one forked from the parent's agent goes below that workspace.
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

/// The workspace whose agent `--fork` forks, `workspace` or else the
/// current one, the agent that starts from the fork, `agent` or else that
/// one, and the session it forks. An agent that cannot start from a fork
/// is refused before anything is created.
async fn forked(
    ctx: &Context,
    workspace: Option<SpaceSelector>,
    agent: Option<Agent>,
) -> Result<(SpaceSelector, Agent, AgentSessionId)> {
    let parent = ui::select_workspace(ctx, workspace, Fallback::CurrentDirectory).await?;
    let AgentSession {
        agent: running,
        session,
        ..
    } = crate::client::agents::session(&ctx.paths, parent.clone()).await?;
    let agent = match agent {
        Some(agent) => agent,
        None => running.name().parse()?,
    };
    let target = ConfigTarget::Workspace(parent.clone());
    let settings = crate::client::workspaces::settings(&ctx.paths, target).await?;
    crate::agents::ensure_forks(&settings, &agent)?;
    Ok((parent, agent, session))
}

/// The positional argument and the repository a workspace's start takes:
/// the `checked` one, in place of the argument that named it, else the one
/// `--repo` names.
fn named_repository(
    positional: Option<StartTarget>,
    repository: Option<String>,
    checked: Option<crate::repository::Selector>,
) -> Result<(Option<StartTarget>, Option<crate::repository::Selector>)> {
    let Some(checked) = checked else {
        return Ok((
            positional,
            repository.map(ui::repository_selector).transpose()?,
        ));
    };
    let positional =
        positional.filter(|positional| !matches!(positional, StartTarget::Repository(_)));
    Ok((positional, Some(checked)))
}

/// Start the agent in a chat in a repository's checkout, or with `--in` in
/// a workspace's worktree.
async fn in_chat(ctx: &Context, mut args: StartArgs, mode: ModeChoice) -> Result<i32> {
    let options = chat::Options::taken_from(&mut args);
    in_chat_with(ctx, args, mode, options).await
}

/// [`in_chat`] with the `options` taken from `args` already.
async fn in_chat_with(
    ctx: &Context,
    args: StartArgs,
    mode: ModeChoice,
    options: chat::Options,
) -> Result<i32> {
    let (repository, issue) = chat_target(
        args.repository,
        args.repository_override,
        args.issue,
        args.in_workspace.is_some(),
    )?;
    let linked_issue = (args.linked_issue.as_ref())
        .map(|issue| templates::IssueFields::at(issue, args.linked_issue_title));
    let subject = chat::Subject {
        mode,
        issue,
        linked_issue,
    };
    match args.in_workspace {
        Some(workspace) => chat::start_in_workspace(ctx, workspace, subject, options).await,
        None => chat::start(ctx, repository, subject, options).await,
    }
}

/// Start the agent in a chat without a repository, in `root_dir`; it starts
/// on no repository or issue.
async fn outside_repositories(
    ctx: &Context,
    mut args: StartArgs,
    mode: modes::ModeName,
) -> Result<i32> {
    ensure_no_repository(&args, &mode)?;
    let topic = chat::Topic::outside_repositories(&mode);
    chat::start_outside_repositories(ctx, topic, chat::Options::taken_from(&mut args)).await
}

/// Refuse a repository or issue `args` names for `mode`, which starts a chat
/// without a repository.
fn ensure_no_repository(args: &StartArgs, mode: &modes::ModeName) -> Result<()> {
    let names = [
        ("<repository>", args.repository.is_some()),
        ("--repo", args.repository_override.is_some()),
        ("--issue", args.issue.is_some()),
    ];
    if let Some((flag, _)) = names.iter().find(|(_, set)| *set) {
        anyhow::bail!(
            "{flag} does not apply to mode {mode}, which starts a chat without a repository"
        );
    }
    Ok(())
}

/// The repository and issue a chat starts on, from the positional argument,
/// `--repo` and `--issue`: a positional issue number or URL is the issue, as
/// it is for a workspace, and a positional repository is the repository. A
/// PR or branch link, two of the same, or a repository beside `--in`, whose
/// workspace has one, is refused.
fn chat_target(
    positional: Option<StartTarget>,
    repository: Option<String>,
    issue: Option<Option<ItemInput>>,
    in_workspace: bool,
) -> Result<(Option<String>, Option<Option<ItemInput>>)> {
    let (repository, issue) = match positional {
        Some(positional) => positional_target(positional, repository, issue)?,
        None => (repository, issue),
    };
    ensure!(
        !in_workspace || repository.is_none(),
        "a chat with --in starts in the workspace's repository; drop the repository"
    );
    Ok((repository, issue))
}

/// [`chat_target`] with a `positional` argument, before the check against
/// `--in`.
fn positional_target(
    positional: StartTarget,
    repository: Option<String>,
    issue: Option<Option<ItemInput>>,
) -> Result<(Option<String>, Option<Option<ItemInput>>)> {
    // A chat takes a positional number as its issue.
    match positional.decided(false)? {
        StartTarget::Pull(_) | StartTarget::Branch { .. } => anyhow::bail!(
            "a chat starts on an issue, not a PR or branch link; start it in a mode that starts in a workspace, such as -m worker"
        ),
        StartTarget::Issue(positional) => {
            ensure!(
                issue.is_none(),
                "name the issue once, positionally or with --issue"
            );
            Ok((repository, Some(Some(positional))))
        }
        StartTarget::Repository(positional) | StartTarget::Number(positional) => {
            ensure!(
                repository.is_none(),
                "a positional repository cannot be combined with --repo"
            );
            Ok((Some(positional), issue))
        }
    }
}

/// The permissions a start's `--permission` assignments give its agent,
/// among which [`ensure_nothing_dropped`] refused a `KEY=mode`.
fn given(changes: &[PermissionChange]) -> PermissionsConfig {
    PermissionChange::combined(changes).set
}

#[cfg(test)]
mod tests;
