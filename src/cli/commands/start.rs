//! `shoal start`: start an agent in a mode, which decides what the agent is
//! told, whether it starts in a repository and, unless the command says
//! otherwise, whether it starts in a workspace or a chat.
use anyhow::{Context as _, Result, ensure};

use super::{
    chat,
    links::{self, AddInput},
    workspaces,
};
use crate::client::space_names::SpaceSelector;
use crate::{
    agent::Agent,
    cli::{
        args::create::StartArgs,
        context::Context,
        ui::{self, Fallback, ModeChoice},
    },
    config::{self, Effective, modes, permissions::PermissionsConfig, templates},
    forge::{
        IssueInput,
        items::link::{Link, LinkTarget},
    },
    model::{AgentSession, SpaceKind},
    protocol::ConfigTarget,
    start_plan::{CopyCreation, Creation},
};

pub(super) async fn run(ctx: &Context, mut args: StartArgs) -> Result<i32> {
    // Without its token, the daemon would take a workspace process's start
    // for the user's and create a space outside its own.
    ensure!(
        crate::env::is_scoped() || !crate::env::inherits_scope(&ctx.paths.state),
        "workspace processes start agents only with their scope token"
    );
    if let Some(parent) = &args.parent {
        args.parent = Some(SpaceSelector::id(&space_id(ctx, parent).await?));
    }
    ensure_copy_fits(&args)?;
    match start_of(ctx, &mut args).await? {
        Start::Workspace(mode) => in_workspace(ctx, args, mode).await,
        Start::Chat(mode) => in_chat(ctx, args, mode).await,
        Start::Outside(mode) => outside_repositories(ctx, args, mode).await,
    }
}

/// Refuse the flags `args` sets that the kind of space `mode`, or the
/// other flags, start in cannot use, before anything is asked or created.
fn ensure_fits(args: &StartArgs, kind: SpaceKind, mode: Option<&str>) -> Result<()> {
    let (unfit, hint) = match kind {
        SpaceKind::Chat => (workspace_flags(args), "pass --worktree for a workspace"),
        SpaceKind::Workspace => (
            args.name
                .is_some()
                .then_some("--name")
                .into_iter()
                .collect(),
            "pass --chat for a chat",
        ),
    };
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

/// The flags `args` sets that only a workspace takes.
fn workspace_flags(args: &StartArgs) -> Vec<&'static str> {
    [
        ("<branch>", args.branch.is_some()),
        ("--existing", args.existing.is_some()),
        ("--path", args.path.is_some()),
        ("--base", args.base.is_some()),
        ("--git-profile", args.git_profile.is_some()),
        ("--fork", args.fork),
        ("--copy", args.copy.copy.is_some()),
        ("--checkpoint", args.copy.checkpoint.is_some()),
        ("--focus", args.focus),
        ("--now", args.now),
        ("--after", !args.after.is_empty()),
        ("--skip-setup", args.creation.skip_setup),
        ("--disposable", args.creation.disposable),
    ]
    .into_iter()
    .filter_map(|(flag, set)| set.then_some(flag))
    .collect()
}

/// The ID of the space `selector` names, by number, ID or name.
async fn space_id(ctx: &Context, selector: &SpaceSelector) -> Result<String> {
    let space = crate::client::workspaces::spaces(&ctx.paths)
        .await?
        .find(selector);
    Ok(space
        .with_context(|| format!("no space {selector}"))?
        .id()
        .to_owned())
}

/// Where a start goes, and in which mode.
enum Start {
    Workspace(ModeChoice),
    /// A chat in a repository.
    Chat(ModeChoice),
    /// A chat without a repository, in the mode named or picked here.
    Outside(String),
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
        None if !ui::can_pick(ctx) => Some(default_mode(args).to_owned()),
        None if args.issue == Some(None) => Some(modes::PLAN.to_owned()),
        None => None,
    };
    if let Some(mode) = named {
        return shaped(ctx, args, mode).await;
    }
    let forced = forced_kind(args);
    if let Some(kind) = forced.or_else(|| fitting_kind(args)) {
        ensure_fits(args, kind, None)?;
        let pick = ModeChoice::Pick { forced };
        return Ok(match kind {
            SpaceKind::Workspace => Start::Workspace(pick),
            SpaceKind::Chat => Start::Chat(pick),
        });
    }
    let mode = picked_mode(ctx, args).await?;
    shaped(ctx, args, mode).await
}

/// Where a start in `mode` goes, by its [`shape`], refusing the flags
/// that kind of space cannot use.
async fn shaped(ctx: &Context, args: &StartArgs, mode: String) -> Result<Start> {
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
) -> Result<Option<String>> {
    let Some(input) = named_issue(args) else {
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
    Ok(mode
        .filter(|mode| modes::get(&settings.modes, mode).is_ok())
        .map(str::to_owned))
}

/// The mode for an issue labelled `labels`: the worker once `[issue]
/// planned_label` says its plan is settled, else the plan mode while
/// `plan_label` says it needs one.
fn labelled_mode(config: &config::Issue, labels: &[String]) -> Option<&'static str> {
    let labelled =
        |label: &Option<String>| label.as_ref().is_some_and(|label| labels.contains(label));
    if labelled(&config.planned_label) {
        Some(modes::DEFAULT)
    } else if labelled(&config.plan_label) {
        Some(modes::PLAN)
    } else {
        None
    }
}

/// The issue `--issue` or the positional argument names by number or URL.
fn named_issue(args: &StartArgs) -> Option<&str> {
    match (&args.issue, args.repository.as_deref()) {
        (Some(Some(issue)), _) => Some(issue),
        (_, Some(positional)) if positional_kind(positional) == Positional::Issue => {
            Some(positional)
        }
        _ => None,
    }
}

/// What the positional argument names.
#[derive(Debug, PartialEq, Eq)]
enum Positional {
    /// An issue, by number or URL.
    Issue,
    /// A PR or branch, by its link.
    Branch,
    /// A repository, by name or URL.
    Repository,
}

fn positional_kind(positional: &str) -> Positional {
    match IssueInput::parse(positional) {
        IssueInput::Number => Positional::Issue,
        IssueInput::Url => match Link::parse(positional) {
            Ok(link) if matches!(link.target, LinkTarget::Issue) => Positional::Issue,
            Ok(_) => Positional::Branch,
            // A repository's own URL.
            Err(_) => Positional::Repository,
        },
        IssueInput::Invalid => Positional::Repository,
    }
}

/// The kind of space the arguments other than the mode fit: a workspace
/// for a flag only a workspace takes or a PR or branch link, a chat for
/// `--name`.
fn fitting_kind(args: &StartArgs) -> Option<SpaceKind> {
    let links_branch = (args.repository.as_deref())
        .is_some_and(|positional| positional_kind(positional) == Positional::Branch);
    if links_branch || !workspace_flags(args).is_empty() {
        Some(SpaceKind::Workspace)
    } else if args.name.is_some() {
        Some(SpaceKind::Chat)
    } else {
        None
    }
}

/// The mode picked for a start whose arguments fit either kind of space,
/// once the agent and model, and any repository or issue it names with its
/// parent, are checked: the [`issue_mode`] of an issue it names, else one
/// picked among the modes in a repository when it names one, else among
/// them all, by the current repository's settings or else the global
/// config's.
async fn picked_mode(ctx: &Context, args: &mut StartArgs) -> Result<String> {
    let names_repository = [&args.repository, &args.repository_override]
        .iter()
        .any(|named| named.is_some())
        || args.issue.is_some();
    let selector = match names_repository {
        true => Some(checked_repository(ctx, args).await?),
        false => None,
    };
    let settings = match &selector {
        Some(selector) => {
            let target = ConfigTarget::Repository(selector.clone());
            crate::client::workspaces::settings(&ctx.paths, target).await?
        }
        None => mode_settings(ctx, args).await?,
    };
    let model = args.model.clone().into();
    chat::checked_agent(&settings, args.agent.clone(), &model)?;
    if let Some(selector) = selector
        && let Some(mode) = issue_mode(ctx, args, selector, &settings).await?
    {
        return Ok(mode);
    }
    let fit = ui::ModeFit {
        kind: None,
        repository: names_repository.then_some(true),
    };
    ModeChoice::Pick { forced: None }.chosen(ctx, &settings.modes, fit)
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
    let parent = args.parent.as_ref().map(ToString::to_string);
    let parent = parent.as_deref();
    let selector =
        workspaces::check_target(ctx, repository.clone(), issue.as_deref(), parent).await?;
    let named = Some(selector.value().to_owned());
    match repository.is_some() && repository == args.repository {
        true => args.repository = named,
        false => args.repository_override = named,
    }
    Ok(selector)
}

/// The mode without `--mode`: chat for a start that asks for a chat, whose
/// agent the worker mode would not tell it is one, else the worker.
fn default_mode(args: &StartArgs) -> &'static str {
    match args.chat || args.in_workspace.is_some() {
        true => modes::CHAT,
        false => modes::DEFAULT,
    }
}

/// The kind of space the start creates, what `--chat`, `--in`,
/// `--worktree`, `--fork` or a copy says, else the kind of `mode`, which
/// must exist before anything is created, and whether `mode` starts it in a
/// repository. The default mode always exists and always starts in a
/// workspace, and a chat in a workspace's worktree belongs to its
/// repository, so neither asks the daemon; a chat's start checks a mode
/// found nowhere here itself once it has the repository.
async fn shape(ctx: &Context, args: &StartArgs, mode: &str) -> Result<(SpaceKind, bool)> {
    if args.in_workspace.is_some() {
        return Ok((SpaceKind::Chat, true));
    }
    let asks_chat = args.chat;
    if mode == modes::DEFAULT && !asks_chat {
        return Ok((SpaceKind::Workspace, true));
    }
    let settings = mode_settings(ctx, args).await?;
    let found = modes::get(&settings.modes, mode);
    if asks_chat {
        return Ok((SpaceKind::Chat, found.map_or(true, |mode| mode.repository)));
    }
    let found = found?;
    let forced = args.worktree || args.fork || args.copy.is_set();
    ensure!(
        found.repository || !forced,
        "mode {mode} starts a chat without a repository; leave out --worktree, --fork, --copy and --checkpoint"
    );
    Ok(match forced {
        true => (SpaceKind::Workspace, true),
        false => (found.kind, found.repository),
    })
}

/// The kind of space `--chat`, `--in`, `--worktree`, `--fork` or a copy
/// asks for, whatever the mode's kind.
fn forced_kind(args: &StartArgs) -> Option<SpaceKind> {
    if args.chat || args.in_workspace.is_some() {
        Some(SpaceKind::Chat)
    } else if args.worktree || args.fork || args.copy.is_set() {
        Some(SpaceKind::Workspace)
    } else {
        None
    }
}

/// The settings whose modes decide the kind: the repository's
/// [`mode_repository`] finds, else the global config's. Nothing is asked.
async fn mode_settings(ctx: &Context, args: &StartArgs) -> Result<Effective> {
    match mode_repository(ctx, args).await? {
        Some(id) => {
            crate::client::workspaces::settings(&ctx.paths, ConfigTarget::Repository(id.into()))
                .await
        }
        None => {
            crate::config::Config::load_with_templates(&ctx.paths)?.resolve(&Default::default())
        }
    }
}

/// The ID of the registered repository `--repo` or the positional argument
/// names, else of the one an issue or item URL belongs to, else of the
/// current checkout or workspace, without asking.
async fn mode_repository(ctx: &Context, args: &StartArgs) -> Result<Option<String>> {
    let repos = crate::client::repositories::list(&ctx.paths).await?;
    let named = (args.repository_override.as_deref()).or(args.repository.as_deref());
    if let Some(Ok(selector)) = named.map(|named| ui::repository_selector(named.to_owned()))
        && let Ok(repo) = crate::repository::select(&repos, selector).await
    {
        return Ok(Some(repo.id.clone()));
    }
    let urls = [args.issue.clone().flatten(), args.repository.clone()];
    for url in urls.iter().flatten() {
        if let Ok(Some(repo)) = super::issues::registered_for_item(&repos, url).await {
            return Ok(Some(repo.id.clone()));
        }
    }
    super::issues::current_repository_id(ctx, &repos).await
}

/// Start the agent in a new or reopened workspace.
async fn in_workspace(ctx: &Context, args: StartArgs, mode: ModeChoice) -> Result<i32> {
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
        false => (
            args.parent.map(|parent| parent.to_string()),
            args.agent,
            None,
        ),
    };
    // A copy goes in the copied workspace's repository unless one is named,
    // which the daemon refuses when it is another.
    let repository = match (&copy, &args.repository) {
        (Some(copy), None) => args
            .repository_override
            .or_else(|| Some(copy.repository.clone())),
        _ => args.repository_override,
    };
    let input = AddInput {
        positional: args.repository,
        repository,
        issue,
    };
    let creation = Creation {
        path: args.path,
        branch: args.branch,
        existing: args.existing,
        base: args.base,
        git_profile: args.git_profile,
        pr: None,
        options: args.creation.into(),
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
        after: args.after,
        model: args.model.into(),
        mode: Some(mode),
        narrowing: PermissionsConfig::combined(&args.narrowing),
        prompt: None,
        parent,
        fork,
    };
    links::add(ctx, input, creation, agent, args.args, options).await
}

/// Refuse a copy beside a PR or branch link, which would open that branch
/// instead, before anything is asked or looked up.
fn ensure_copy_fits(args: &StartArgs) -> Result<()> {
    let links_branch = (args.repository.as_deref())
        .is_some_and(|positional| positional_kind(positional) == Positional::Branch);
    ensure!(
        !args.copy.is_set() || !links_branch,
        "a copy starts from the workspace it copies, not a PR or branch link"
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
) -> Result<(Option<String>, Option<Agent>, Option<String>)> {
    if copied.is_some() {
        let (_, agent, session) = forked(ctx, copied, agent).await?;
        return Ok((
            parent.map(|parent| parent.to_string()),
            Some(agent),
            Some(session),
        ));
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
) -> Result<(String, Agent, String)> {
    let parent = ui::select_workspace(ctx, workspace, Fallback::CurrentDirectory).await?;
    let AgentSession {
        agent: running,
        session,
        ..
    } = crate::client::agents::session(&ctx.paths, SpaceSelector::parse(&parent)).await?;
    let agent = match agent {
        Some(agent) => agent,
        None => running.parse()?,
    };
    let target = ConfigTarget::Workspace(SpaceSelector::parse(&parent));
    let settings = crate::client::workspaces::settings(&ctx.paths, target).await?;
    crate::agents::ensure_forks(&settings, agent.name())?;
    Ok((parent, agent, session))
}

/// Start the agent in a chat in a repository's checkout, or with `--in` in
/// a workspace's worktree.
async fn in_chat(ctx: &Context, mut args: StartArgs, mode: ModeChoice) -> Result<i32> {
    let options = chat_options(&mut args);
    let (repository, issue) = chat_target(
        args.repository,
        args.repository_override,
        args.issue,
        args.in_workspace.is_some(),
    )?;
    let linked_issue = (args.linked_issue.as_deref())
        .and_then(|url| templates::IssueFields::at(url, args.linked_issue_title));
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
async fn outside_repositories(ctx: &Context, mut args: StartArgs, mode: String) -> Result<i32> {
    ensure_no_repository(&args, &mode)?;
    let topic = chat::Topic::outside_repositories(&mode);
    chat::start_outside_repositories(ctx, topic, chat_options(&mut args)).await
}

/// Refuse a repository or issue `args` names for `mode`, which starts a chat
/// without a repository.
fn ensure_no_repository(args: &StartArgs, mode: &str) -> Result<()> {
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

/// How `args` start a chat, taken from them.
fn chat_options(args: &mut StartArgs) -> chat::Options {
    chat::Options {
        agent: args.agent.take(),
        place: chat::ChatPlace::here(args.here),
        name: args.name.take(),
        model: std::mem::take(&mut args.model).into(),
        args: std::mem::take(&mut args.args),
        session: args.session.take(),
        fork: args.fork_session,
        restore: args.restore.take(),
        handoff: args.handoff.take(),
        parent: args.parent.take().map(|parent| parent.to_string()),
        narrowing: match args.recorded_narrowing.take() {
            Some(recorded) => chat::Narrowing::Recorded(recorded),
            None => chat::Narrowing::Requested(PermissionsConfig::combined(&args.narrowing)),
        },
    }
}

/// The repository and issue a chat starts on, from the positional argument,
/// `--repo` and `--issue`: a positional issue number or URL is the issue, as
/// it is for a workspace, and a positional repository is the repository. A
/// PR or branch link, two of the same, or a repository beside `--in`, whose
/// workspace has one, is refused.
fn chat_target(
    positional: Option<String>,
    repository: Option<String>,
    issue: Option<Option<String>>,
    in_workspace: bool,
) -> Result<(Option<String>, Option<Option<String>>)> {
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
    positional: String,
    repository: Option<String>,
    issue: Option<Option<String>>,
) -> Result<(Option<String>, Option<Option<String>>)> {
    let kind = positional_kind(&positional);
    ensure!(
        kind != Positional::Branch,
        "a chat starts on an issue, not a PR or branch link; pass --worktree for a workspace"
    );
    if kind == Positional::Issue {
        ensure!(
            issue.is_none(),
            "name the issue once, positionally or with --issue"
        );
        return Ok((repository, Some(Some(positional))));
    }
    ensure!(
        repository.is_none(),
        "a positional repository cannot be combined with --repo"
    );
    Ok((Some(positional), issue))
}

#[cfg(test)]
mod tests;
