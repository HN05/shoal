//! `shoal start`: start an agent in a mode, which decides what the agent is
//! told and, unless the command says otherwise, whether it starts in a
//! workspace or a chat.
use anyhow::{Context as _, Result, ensure};

use super::{
    chat,
    links::{self, AddInput},
    workspaces::{self, Creation},
};
use crate::{
    agent::Agent,
    cli::{
        args::create::StartArgs,
        client,
        context::Context,
        ui::{self, Fallback},
    },
    config::{Effective, modes},
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
    let mode = (args.mode.clone()).unwrap_or_else(|| default_mode(&args).to_owned());
    if let Some(parent) = &args.parent {
        args.parent = Some(space_id(ctx, parent).await?);
    }
    match kind(ctx, &args, &mode).await? {
        SpaceKind::Workspace => in_workspace(ctx, args, mode).await,
        SpaceKind::Chat => in_chat(ctx, args, mode).await,
    }
}

/// The ID of the space `selector` names, by ID, name or number.
async fn space_id(ctx: &Context, selector: &str) -> Result<String> {
    let space = client::spaces(&ctx.paths).await?.find_numbered(selector);
    Ok(space
        .with_context(|| format!("no space {selector}"))?
        .id()
        .to_owned())
}

/// The mode without `--mode`: chat for a start that asks for a chat, whose
/// agent the worker mode would not tell it is one, else the worker.
fn default_mode(args: &StartArgs) -> &'static str {
    match args.chat || args.in_workspace.is_some() {
        true => chat::CHAT_MODE,
        false => modes::DEFAULT,
    }
}

/// The kind of space the start creates: what `--chat`, `--in`,
/// `--worktree` or `--fork` says, else the kind of `mode`, which must exist
/// before anything is created. The default mode always does and always
/// starts in a workspace, so only another mode's start asks the daemon; a
/// chat's start checks its mode itself once it has the repository.
async fn kind(ctx: &Context, args: &StartArgs, mode: &str) -> Result<SpaceKind> {
    if args.chat || args.in_workspace.is_some() {
        return Ok(SpaceKind::Chat);
    }
    let forced = args.worktree || args.fork;
    if mode == modes::DEFAULT {
        return Ok(SpaceKind::Workspace);
    }
    let settings = mode_settings(ctx, args).await?;
    let kind = modes::get(&settings.modes, mode)?.kind;
    Ok(if forced { SpaceKind::Workspace } else { kind })
}

/// The settings whose modes decide the kind: the repository's
/// [`mode_repository`] finds, else the global config's. Nothing is asked.
async fn mode_settings(ctx: &Context, args: &StartArgs) -> Result<Effective> {
    match mode_repository(ctx, args).await? {
        Some(id) => client::settings(&ctx.paths, ConfigTarget::Repository(id.into())).await,
        None => {
            crate::config::Config::load_with_templates(&ctx.paths)?.resolve(&Default::default())
        }
    }
}

/// The ID of the registered repository `--repo` or the positional argument
/// names, else of the one an issue or item URL belongs to, else of the
/// current checkout or workspace, without asking.
async fn mode_repository(ctx: &Context, args: &StartArgs) -> Result<Option<String>> {
    let repos = client::repositories(&ctx.paths).await?;
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
    let issue = match args.issue {
        Some(None) => anyhow::bail!("--issue needs a number or URL outside plan mode"),
        Some(Some(issue)) => Some(issue),
        None => None,
    };
    let (parent, agent, fork) = match args.fork {
        true => {
            let (parent, session) = forked(ctx, args.parent).await?;
            let agent = match args.agent {
                Some(agent) => agent,
                None => session.agent.parse::<Agent>()?,
            };
            (Some(parent), Some(agent), Some(session.session))
        }
        false => (args.parent, args.agent, None),
    };
    let input = AddInput {
        positional: args.repository,
        repository: args.repository_override,
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

/// The workspace whose agent `--fork` forks, `parent` or else the current
/// one, with the agent's session.
async fn forked(ctx: &Context, parent: Option<String>) -> Result<(String, AgentSession)> {
    let parent = ui::select_workspace(ctx, parent, Fallback::CurrentDirectory).await?;
    let session = crate::client::agents::session(&ctx.paths, parent.clone()).await?;
    Ok((parent, session))
}

/// Start the agent in a chat in a repository's checkout, or with `--in` in
/// a workspace's worktree.
async fn in_chat(ctx: &Context, args: StartArgs, mode: String) -> Result<i32> {
    ensure!(
        args.after.is_empty() && !args.now,
        "--after and --now queue agents of workspaces, and a chat starts at once"
    );
    let options = chat::Options {
        agent: args.agent,
        place: chat::ChatPlace::here(args.here),
        name: args.name,
        model: args.model.into(),
        args: args.args,
        session: args.session,
        restore: args.restore,
        handoff: args.handoff,
        parent: args.parent,
    };
    let subject = chat::Subject {
        mode,
        issue: args.issue,
    };
    match args.in_workspace {
        Some(workspace) => chat::start_in_workspace(ctx, workspace, subject, options).await,
        None => {
            let repository = args.repository_override.or(args.repository);
            chat::start(ctx, repository, subject, options).await
        }
    }
}
