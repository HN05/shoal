//! Starting the agent of a space that changed since its session stopped, the
//! same way for a workspace and a chat.
use anyhow::{Context as _, Result};
use std::path::PathBuf;

use super::reports;
use crate::{
    cli::context::Context,
    config::templates::Subject,
    execution::recovery::{LastSession, LastSessionExt as _, changes::Change},
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

/// From a fork of its session where it may and its agent can, otherwise as a
/// new agent told how the space changed. It consumes `records` once it has
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
    reports::starting_changed(&session.agent, space.kind(), &change, fork.is_some());
    let start = ChangedStart {
        told: change.told(space, fork.is_some(), session.ending().mid_turn(), handoff),
        session,
        change,
        fork,
        records,
    };
    launch_in(ctx, space, start).await
}

/// A handoff's new tool takes the model and effort asked of it over its
/// configured ones; any other agent takes those it ran with.
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
    let model = match change {
        Change::HandedOver(_) => shoal_workers::launch::ModelRequest::Override,
        _ => shoal_workers::launch::ModelRequest::Kept,
    };
    let options = shoal_workers::launch::LaunchOptions {
        replaces: records,
        fork,
        model: model(session.model.clone()),
        narrowing: session.narrowing,
    };
    let mode = session.mode.clone();
    let opening = match change {
        Change::HandedOver(brief) => shoal_workers::launch::Opening::New {
            mode,
            subject: Some(told),
            note: brief.note,
        },
        Change::Converted => shoal_workers::launch::Opening::New {
            mode,
            subject: Some(told),
            note: None,
        },
        Change::Copied(_) | Change::Moved => shoal_workers::launch::Opening::Told {
            mode,
            message: told.text,
        },
    };
    let records = shoal_workers::homes::records(&ctx.paths, &SpaceSelector::id(&space.id)).await?;
    shoal_workers::launch::launch_agent(ctx.caller(), launch, records, opening, vec![], options)
        .await
}
