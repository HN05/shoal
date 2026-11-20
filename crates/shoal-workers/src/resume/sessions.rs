//! Starting stopped agents at once, each where it stopped, through the
//! wrapper that waits for it, or in the background, leaving those nothing
//! there can start for terminals of the user's own.
use anyhow::Result;
use shoal_core::execution::recovery::SavedSession;
use shoal_ids::execution_ids::ExecutionId;
use shoal_ids::space_ids::SpaceId;
use std::ffi::OsString;

use super::{Resuming, places, queue};
use crate::{
    client::{
        connection::CommandPaths,
        output::resume::{RestoredAgent, StartedRestores},
    },
    model::Space,
    places::Launcher,
    protocol::ConfigTarget,
};
use shoal_protocol::space_names::SpaceSelector;

/// What a start of stopped agents at once did.
pub enum Starts {
    /// Nothing beside the caller could start the only agent, `execution`
    /// of the space `space`, which the caller restores in its own terminal.
    Here {
        space: SpaceId,
        execution: ExecutionId,
    },
    /// Each agent started or failed to, as [`Started`] reports.
    Elsewhere(Started),
}

/// The agents a start at once started, as `output` lists them.
pub struct Started {
    pub output: StartedRestores,
    /// How many of them started where they wait to restore.
    pub in_wrappers: usize,
    /// What those it started elsewhere run in, when any.
    pub place: Option<&'static str>,
    /// The ones the user starts in terminals of their own, by space.
    pub listed: Vec<(Space, ExecutionId)>,
}

/// Start each of the stopped agents `sessions` lists, by space, through
/// the wrapper that waits for it, where it stopped or in the background,
/// as `launcher` starts work, with what `output` holds already; `single`
/// leaves the only one to the caller's terminal when none is possible.
/// `resuming` tells of each that fails.
pub async fn start(
    paths: &CommandPaths,
    launcher: Launcher,
    sessions: Vec<(Space, SavedSession)>,
    single: bool,
    output: StartedRestores,
    resuming: &impl Resuming,
) -> Result<Starts> {
    let mut started = Started {
        output,
        in_wrappers: 0,
        place: None,
        listed: Vec::new(),
    };
    for (space, session) in sessions {
        let start = start_session(paths, launcher, &space, &session, single).await;
        let id = session.id;
        let (reused, place) = match start {
            Ok(Start::Here) => {
                return Ok(Starts::Here {
                    space: space.id,
                    execution: id,
                });
            }
            Ok(Start::Elsewhere { reused, place }) => (reused, place),
            Ok(Start::InWrapper) => {
                started.in_wrappers += 1;
                started.output.agents.push(RestoredAgent {
                    workspace: space.name.clone(),
                    execution: id,
                    background: false,
                    reused: true,
                });
                continue;
            }
            Err(error) => {
                let failure = resuming.failed(&space.name, &error);
                started.output.failed.push(failure);
                continue;
            }
        };
        started.place = place.or(started.place);
        started.output.agents.push(RestoredAgent {
            workspace: space.name.clone(),
            execution: id.clone(),
            background: place.is_some(),
            reused,
        });
        if place.is_none() {
            started.listed.push((space, id));
        }
    }
    Ok(Starts::Elsewhere(started))
}

/// Where a stopped agent resumes.
enum Start {
    /// Through the wrapper that waits to restore it, which its queued
    /// restore lets go.
    InWrapper,
    /// In the caller's terminal, as the only agent to resume where nothing
    /// beside it can start one.
    Here,
    /// Where it stopped when `reused`, in the background, or, when `place`
    /// is `None`, in a terminal of the user's own.
    Elsewhere {
        reused: bool,
        place: Option<&'static str>,
    },
}

/// Start the stopped agent `session` of `space` through the wrapper that
/// waits for it, where it stopped or in the background, as `launcher`
/// starts work; when none is possible, `single` leaves the only one to the
/// caller's terminal.
async fn start_session(
    paths: &CommandPaths,
    launcher: Launcher,
    space: &Space,
    session: &SavedSession,
    single: bool,
) -> Result<Start> {
    if session.waits_in_wrapper {
        queue::queue(paths, space, &session.id, true).await?;
        return Ok(Start::InWrapper);
    }
    let target = ConfigTarget::Space(SpaceSelector::id(&space.id));
    let settings = crate::settings::of(paths, target).await?;
    let host = launcher.host(&settings);
    let args = [
        "resume",
        space.id.as_str(),
        "--execution",
        session.id.as_str(),
        "--focus",
    ]
    .map(OsString::from);
    let stopped = session.origin.as_ref();
    if let Some(place) = places::rerun_where_stopped(paths, &host, stopped, &args).await? {
        return Ok(Start::Elsewhere {
            reused: true,
            place: Some(place),
        });
    }
    let place = places::run_in_background(paths, (&settings, &host), space, &args).await?;
    if single && place.is_none() {
        return Ok(Start::Here);
    }
    Ok(Start::Elsewhere {
        reused: false,
        place,
    })
}
