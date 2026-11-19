//! Starting the agent of a space that changed since its session stopped,
//! a workspace's or a chat's: what it is told, whether it starts from a
//! fork of its session and its launch are the same for either kind.
use anyhow::{Context as _, Result};
use std::path::PathBuf;

use super::reports;
use crate::{
    cli::context::Context,
    config::templates::Subject,
    execution::recovery::{LastSession, changes::Change},
    model::Space,
    protocol::ConfigTarget,
};
use shoal_ids::agent_session_ids::AgentSessionId;
use shoal_protocol::space_names::SpaceSelector;

/// How the agent of a changed space starts: as `session` saved it, told
/// `told`, from a fork of the session `fork` when given, replacing
/// `records`, which it consumes once it has started.
struct ChangedStart {
    session: LastSession,
    change: Change,
    fork: Option<AgentSessionId>,
    told: Subject,
    records: Vec<PathBuf>,
}

/// Start the agent of `session` in `space`, which changed as `change` says
/// since it stopped: from a fork of its session where it may and its agent
/// can, otherwise as a new agent, told how the space changed and about the
/// stopped commands `handoff` describes; it consumes `records` once it has
/// started.
pub(super) async fn start_changed(
    ctx: &Context,
    space: &Space,
    session: LastSession,
    change: Change,
    handoff: Option<&str>,
    records: Vec<PathBuf>,
) -> Result<i32> {
    let target = ConfigTarget::Space(SpaceSelector::id(&space.id));
    let settings = shoal_workers::settings::of(&ctx.paths, target).await?;
    let forks = change.forks() && crate::agents::ensure_forks(&settings, &session.agent).is_ok();
    let fork = session.session.clone().filter(|_| forks);
    reports::starting_changed(&session.agent, space.kind(), change, fork.is_some());
    let start = ChangedStart {
        told: change.told(space, fork.is_some(), handoff),
        session,
        change,
        fork,
        records,
    };
    launch_in(ctx, space, start).await
}

/// Launch the agent `start` describes in `space`, as a new agent is
/// started, with the model and effort it ran with: a converted one starts
/// on what it is told and its mode's instructions, any other is told it as
/// it is.
async fn launch_in(ctx: &Context, space: &Space, start: ChangedStart) -> Result<i32> {
    let ChangedStart {
        session,
        change,
        fork,
        told,
        records,
    } = start;
    let agent = &session.agent;
    let launch: crate::agent::Agent = agent
        .name()
        .parse()
        .with_context(|| format!("cannot start agent {agent} of {}", space.name))?;
    let options = crate::cli::agents::LaunchOptions {
        replaces: records,
        fork,
        model: crate::cli::agents::ModelRequest::Kept(session.model.clone()),
        narrowing: session.narrowing,
    };
    let mode = session.mode.clone();
    let opening = match change {
        Change::Converted => crate::cli::agents::Opening::New {
            mode,
            subject: Some(told),
        },
        Change::Copied(_) | Change::Moved => crate::cli::agents::Opening::Told {
            mode,
            message: told.text,
        },
    };
    let records = shoal_workers::homes::records(&ctx.paths, &SpaceSelector::id(&space.id)).await?;
    crate::cli::agents::launch_agent(ctx, launch, records, opening, vec![], options).await
}
