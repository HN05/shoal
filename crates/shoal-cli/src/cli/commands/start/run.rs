//! Running here the agent of a recorded space's starting launch, which
//! this process holds: a workspace's once it is open and set up, a chat's
//! at once, both through one launch of the space's agent.
use anyhow::{Context as _, Result};
use std::ffi::OsString;

use crate::{
    agents::HomeRecords,
    cli::{
        agents::{self, LaunchOptions, ModelRequest, Opening},
        commands::{issues, queued_agents::Hold, workspaces},
        context::Context,
        terminal,
    },
    config::{modes::ModeName, templates::Subject},
    protocol::{ItemContext, operations::QueuedLaunch},
    start_plan::QueuedStart,
    terminal::{Owner, PlaceName},
};
use shoal_ids::queue_entry_ids::QueueEntryId;
use shoal_protocol::scope_tokens::ScopeToken;
use shoal_protocol::space_names::SpaceSelector;

/// Run here the agent of the starting launch `entry`, which this process
/// holds, in the space it names, the place it runs in named as
/// `place_name` says, failing the entry when its start fails.
pub(in crate::cli) async fn run_entry(
    ctx: &Context,
    entry: &QueueEntryId,
    place_name: Option<PlaceName>,
) -> Result<i32> {
    let _hold = Hold::start(&ctx.paths, entry).await;
    let mut scope = SpaceScope { held: None };
    let started = start_entry(ctx, entry, place_name, &mut scope).await;
    // The space's scope reaches its entry, which the starter's may no
    // longer, so the failure is reported before the scope is revoked.
    if let Err(error) = &started {
        crate::queued_starts::fail(&ctx.paths, entry, error).await;
    }
    scope.leave(ctx).await;
    started
}

/// Start the agent of the starting launch `entry` as [`run_entry`] does,
/// in the `scope` this process issues for its space when it is scoped.
async fn start_entry(
    ctx: &Context,
    entry: &QueueEntryId,
    place_name: Option<PlaceName>,
    scope: &mut SpaceScope,
) -> Result<i32> {
    let queued = crate::client::queue::plan(&ctx.paths, entry.clone()).await?;
    let start = QueuedStart::of(&queued.plan)?;
    let space = (queued.entry.space()).context("the starting launch names no space")?;
    let space = shoal_workers::homes::records(&ctx.paths, &SpaceSelector::id(space)).await?;
    let (space, entry) = match open(ctx, space, (entry, &start), place_name).await? {
        Opened::Ready(space, entry) => (*space, entry),
        Opened::Ended(code) => return Ok(code),
    };
    *scope = SpaceScope::act_in(ctx, &space).await?;
    launch_in(ctx, space, entry.as_ref(), start.launch).await
}

/// Launch the agent of `launch` in `space`, taking the slot of `entry`.
async fn launch_in(
    ctx: &Context,
    space: HomeRecords,
    entry: Option<&QueueEntryId>,
    launch: QueuedLaunch,
) -> Result<i32> {
    let subject = subject(ctx, &space, &launch).await?;
    if let Some(entry) = entry {
        crate::env::launch_queue_entry(entry);
    }
    self::launch(ctx, space, launch, subject).await
}

/// What opening a space for its agent left.
enum Opened {
    /// The space, ready for the agent of the starting launch `entry`.
    Ready(Box<HomeRecords>, Option<QueueEntryId>),
    /// The start ended with this code, as a workspace's failed setup ends
    /// it.
    Ended(i32),
}

/// Open `space` for the agent of the starting launch `entry`'s `start` in
/// the place this process runs in, named as `place_name` says: a workspace
/// once its preparation sets it up, a chat at once.
async fn open(
    ctx: &Context,
    space: HomeRecords,
    agent: (&QueueEntryId, &QueuedStart),
    place_name: Option<PlaceName>,
) -> Result<Opened> {
    let workspace = match space {
        HomeRecords::Workspace(workspace) => workspace,
        HomeRecords::Chat { ref chat, .. } => {
            terminal::host_space(ctx, Owner::Chat(&chat.id), true, &chat.name).await?;
            return Ok(Opened::Ready(Box::new(space), Some(agent.0.clone())));
        }
    };
    let opened = workspaces::open_recorded(ctx, &workspace, Some(agent), place_name).await?;
    Ok(match opened {
        workspaces::Opened::Ready { workspace, entry } => {
            Opened::Ready(Box::new(HomeRecords::Workspace(*workspace)), entry)
        }
        workspaces::Opened::Ended(code) => Opened::Ended(code),
    })
}

/// What the agent of `launch` in `space` starts on: the issue it names,
/// read as the space reads it, open or closed, since its work goes on
/// whatever the issue's state, else its prompt.
async fn subject(
    ctx: &Context,
    space: &HomeRecords,
    launch: &QueuedLaunch,
) -> Result<Option<Subject>> {
    let Some(issue) = &launch.issue else {
        return Ok(launch.prompt.clone().map(Subject::told));
    };
    let context = ItemContext::space(SpaceSelector::id(space.home()?.id()));
    let issue = issues::load_linked(ctx, context, issue.url.clone()).await?;
    Ok(Some(issue.subject()))
}

/// Launch the agent of `launch` in `space`, starting on `subject`.
async fn launch(
    ctx: &Context,
    space: HomeRecords,
    launch: QueuedLaunch,
    subject: Option<Subject>,
) -> Result<i32> {
    let QueuedLaunch {
        agent,
        mode,
        narrowing,
        model,
        args,
        fork,
        ..
    } = launch;
    let opening = Opening::New {
        mode: mode.unwrap_or(ModeName::DEFAULT),
        subject,
    };
    let options = LaunchOptions {
        fork,
        model: ModelRequest::Override(model),
        narrowing,
        ..Default::default()
    };
    let args = args.into_iter().map(OsString::from).collect();
    agents::launch_agent(ctx, agent, space, opening, args, options).await
}

/// The scope a scoped process issues itself for the space whose agent it
/// runs, which that agent outlasts the scope it inherited with.
struct SpaceScope {
    held: Option<(SpaceSelector, ScopeToken)>,
}

impl SpaceScope {
    /// Make this process act in a scope it issues for `space`, when it is
    /// scoped.
    async fn act_in(ctx: &Context, space: &HomeRecords) -> Result<Self> {
        if !crate::env::is_scoped() {
            return Ok(Self { held: None });
        }
        let space = SpaceSelector::id(space.home()?.id());
        let token = crate::client::spaces::issue_scope(&ctx.paths, space.clone()).await?;
        crate::env::act_in_space_scope(token.clone());
        Ok(Self {
            held: Some((space, token)),
        })
    }

    /// Revoke the scope, once the start ended. A token it cannot revoke
    /// goes with the space, as when its removal ended its agent.
    async fn leave(self, ctx: &Context) {
        if let Some((space, token)) = self.held {
            let _ = crate::client::spaces::revoke_scope(&ctx.paths, space, token).await;
        }
    }
}
