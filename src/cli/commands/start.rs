//! `shoal start`: start an agent in a mode, which decides what the agent is
//! told and, unless the command says otherwise, whether it starts in a
//! workspace or a chat.
use std::collections::BTreeMap;

use anyhow::{Context as _, Result, ensure};

use super::{
    chat,
    links::{self, AddInput},
    workspaces::{self, CopyCreation, Creation},
};
use crate::{
    agent::Agent,
    cli::{
        args::create::StartArgs,
        context::Context,
        output::{Palette, Style},
        ui::{self, Fallback},
    },
    config::{
        Effective,
        modes::{self, Mode},
        templates,
    },
    forge::{
        IssueInput,
        items::link::{Link, LinkTarget},
    },
    model::{AgentSession, SpaceKind},
    protocol::ConfigTarget,
};

pub(super) async fn run(ctx: &Context, mut args: StartArgs) -> Result<i32> {
    // Without its token, the daemon would take a workspace process's start
    // for the user's and create a space outside its own.
    ensure!(
        crate::env::is_scoped() || !crate::env::inherits_scope(&ctx.paths.state),
        "workspace processes start agents only with their scope token"
    );
    let mode = mode(ctx, &args).await?;
    if let Some(parent) = &args.parent {
        args.parent = Some(space_id(ctx, parent).await?);
    }
    let kind = kind(ctx, &args, &mode).await?;
    ensure_fits(&args, kind, &mode)?;
    match kind {
        SpaceKind::Workspace => in_workspace(ctx, args, mode).await,
        SpaceKind::Chat => in_chat(ctx, args, mode).await,
    }
}

/// Refuse the flags `args` sets that the kind of space `mode` starts in
/// cannot use, before anything is asked or created.
fn ensure_fits(args: &StartArgs, kind: SpaceKind, mode: &str) -> Result<()> {
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
    anyhow::bail!("{flag} does not apply to a {kind}, which mode {mode} starts in; {hint}")
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

/// The ID of the space `selector` names, by ID, name or number.
async fn space_id(ctx: &Context, selector: &str) -> Result<String> {
    let space = crate::client::workspaces::spaces(&ctx.paths)
        .await?
        .find_numbered(selector);
    Ok(space
        .with_context(|| format!("no space {selector}"))?
        .id()
        .to_owned())
}

/// The mode `--mode` names, else the one picked on a terminal, else the
/// [`default_mode`].
async fn mode(ctx: &Context, args: &StartArgs) -> Result<String> {
    match &args.mode {
        Some(mode) => Ok(mode.clone()),
        None if ui::can_pick(ctx) => {
            let settings = mode_settings(ctx, args).await?;
            let (default, forced) = (default_mode(args), forced_kind(args));
            let entries = mode_entries(&settings.modes, default, forced, Palette::picker());
            ui::pick(ctx, "Mode> ", entries)
        }
        None => Ok(default_mode(args).to_owned()),
    }
}

/// The picker's entries for `modes`: `default` first, so Enter keeps it,
/// then the rest by name, each with the kind of space it would start in,
/// `forced` or else its own.
fn mode_entries(
    modes: &BTreeMap<String, Mode>,
    default: &str,
    forced: Option<SpaceKind>,
    palette: Palette,
) -> ui::Entries {
    let mut modes: Vec<_> = modes.iter().collect();
    modes.sort_by_key(|(name, _)| name.as_str() != default);
    let rows = modes
        .iter()
        .map(|(name, mode)| {
            let kind = forced.unwrap_or(mode.kind);
            [
                ((*name).clone(), None),
                (kind.to_string(), Some(Style::Muted)),
            ]
        })
        .collect();
    let labels = ui::aligned(rows, palette);
    modes
        .into_iter()
        .map(|(name, _)| name.clone())
        .zip(labels)
        .collect()
}

/// The mode without `--mode`: chat for a start that asks for a chat, whose
/// agent the worker mode would not tell it is one, else the worker.
fn default_mode(args: &StartArgs) -> &'static str {
    match args.chat || args.in_workspace.is_some() {
        true => modes::CHAT,
        false => modes::DEFAULT,
    }
}

/// The kind of space the start creates: what [`forced_kind`] says, else
/// the kind of `mode`, which must exist before anything is created. The
/// default mode always does and always starts in a workspace, so only
/// another mode's start asks the daemon; a chat's start checks its mode
/// itself once it has the repository.
async fn kind(ctx: &Context, args: &StartArgs, mode: &str) -> Result<SpaceKind> {
    let forced = forced_kind(args);
    if forced == Some(SpaceKind::Chat) {
        return Ok(SpaceKind::Chat);
    }
    if mode == modes::DEFAULT {
        return Ok(SpaceKind::Workspace);
    }
    let settings = mode_settings(ctx, args).await?;
    let kind = modes::get(&settings.modes, mode)?.kind;
    Ok(forced.unwrap_or(kind))
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
async fn in_workspace(ctx: &Context, args: StartArgs, mode: String) -> Result<i32> {
    ensure_copy_fits(&args)?;
    let issue = match args.issue {
        Some(None) => anyhow::bail!("--issue needs a number or URL outside plan mode"),
        Some(Some(issue)) => Some(issue),
        None => None,
    };
    let copy = super::copies::source(ctx, args.copy).await?;
    let copied = copy.as_ref().map(|copy| copy.source.workspace.clone());
    let (parent, agent, fork) = match args.fork {
        true => fork_from(ctx, copied, args.parent, args.agent).await?,
        false => (args.parent, args.agent, None),
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
        parent,
        fork,
    };
    links::add(ctx, input, creation, agent, args.args, options).await
}

/// Refuse a copy beside a PR or branch link, which would open that branch
/// instead, before anything is asked or looked up.
fn ensure_copy_fits(args: &StartArgs) -> Result<()> {
    let link = (args.repository.as_deref()).and_then(|positional| Link::parse(positional).ok());
    ensure!(
        !args.copy.is_set() || link.is_none_or(|link| matches!(link.target, LinkTarget::Issue)),
        "a copy starts from the workspace it copies, not a PR or branch link"
    );
    Ok(())
}

/// The parent, agent and session of a `--fork` start: one forked from the
/// `copied` workspace's agent goes below `parent` as any start does, while
/// one forked from the parent's agent goes below that workspace.
async fn fork_from(
    ctx: &Context,
    copied: Option<String>,
    parent: Option<String>,
    agent: Option<Agent>,
) -> Result<(Option<String>, Option<Agent>, Option<String>)> {
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
    workspace: Option<String>,
    agent: Option<Agent>,
) -> Result<(String, Agent, String)> {
    let parent = ui::select_workspace(ctx, workspace, Fallback::CurrentDirectory).await?;
    let AgentSession {
        agent: running,
        session,
        ..
    } = crate::client::agents::session(&ctx.paths, parent.clone()).await?;
    let agent = match agent {
        Some(agent) => agent,
        None => running.parse()?,
    };
    let target = ConfigTarget::Workspace(parent.clone());
    let settings = crate::client::workspaces::settings(&ctx.paths, target).await?;
    crate::cli::agents::ensure_forks(&settings, agent.name())?;
    Ok((parent, agent, session))
}

/// Start the agent in a chat in a repository's checkout, or with `--in` in
/// a workspace's worktree.
async fn in_chat(ctx: &Context, args: StartArgs, mode: String) -> Result<i32> {
    let options = chat::Options {
        agent: args.agent,
        place: chat::ChatPlace::here(args.here),
        name: args.name,
        model: args.model.into(),
        args: args.args,
        session: args.session,
        fork: args.fork_session,
        restore: args.restore,
        handoff: args.handoff,
        parent: args.parent,
    };
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
    let names_issue = match IssueInput::parse(&positional) {
        IssueInput::Number => true,
        IssueInput::Url => match Link::parse(&positional) {
            Ok(link) => {
                ensure!(
                    matches!(link.target, LinkTarget::Issue),
                    "a chat starts on an issue, not a PR or branch link; pass --worktree for a \
                     workspace"
                );
                true
            }
            // A repository's own URL.
            Err(_) => false,
        },
        _ => false,
    };
    if names_issue {
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
