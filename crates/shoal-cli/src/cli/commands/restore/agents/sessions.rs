//! Starting stopped agents at once, each where it stopped, through the
//! wrapper that waits for it, in the background, or in this terminal, and
//! listing those the user starts in terminals of their own.
use anyhow::Result;
use std::ffi::OsString;

use super::{commands, queue, reports};
use crate::client::output::resume::{Resumed, ResumedAgent};
use crate::client::space_names::SpaceSelector;
use crate::{
    cli::{context::Context, terminal, ui},
    execution::recovery::{Saved, SavedSession},
    model::PresentSpace,
    protocol::ConfigTarget,
};

/// Resume each space's stopped work, reporting it with what `resumed` holds
/// already. Commands without an agent to receive them, as none restoring
/// through its waiting wrapper does, are reported here. An agent resumes
/// where it stopped when the terminal backend can reuse that place,
/// otherwise in the backend's background; without a backend one agent
/// resumes in this terminal unless `resumed` reports setups or failures,
/// and the others are listed for their own terminals.
pub(super) async fn run_all(
    ctx: &Context,
    stopped: Vec<(PresentSpace, Saved)>,
    mut resumed: Resumed,
) -> Result<i32> {
    let mut sessions = Vec::new();
    for (space, mut saved) in stopped {
        if !saved.restores_outside_wrappers() {
            let _claims = commands::claim(&mut saved.commands);
            match commands::report(ctx, &space, &saved.commands) {
                Ok(report) => resumed.commands.push(report),
                Err(error) => {
                    let failure = ui::failure(ctx, "resume", space.name(), &error);
                    resumed.failed.push(failure);
                }
            }
        }
        sessions.extend(
            saved
                .agents
                .into_iter()
                .chain(saved.sessions)
                .map(|saved| (space.clone(), saved)),
        );
    }
    // An agent restored here replaces this run's report, so it takes the
    // terminal only when there is nothing else to report.
    let alone = resumed.set_up.is_empty() && resumed.failed.is_empty();
    let single = sessions.len() == 1 && alone && ctx.on_terminal();
    start_sessions(ctx, sessions, single, resumed).await
}

/// Start each of the stopped agents `sessions` lists, by space, through the
/// wrapper that waits for it, where it stopped or in the background;
/// `single` resumes the only one in this terminal when none is possible.
/// Reports them with what `output` holds already.
pub(super) async fn start_sessions(
    ctx: &Context,
    sessions: Vec<(PresentSpace, SavedSession)>,
    single: bool,
    mut output: Resumed,
) -> Result<i32> {
    let mut listed = Vec::new();
    let mut in_wrappers = 0;
    let mut place = None;
    for (space, session) in &sessions {
        let id = &session.id;
        let (reused, started) = match start_session(ctx, space, session, single).await {
            Ok(Started::Here) => {
                // Boxed, since `run` calls back into this function.
                let id = Some(id.clone());
                let selector = SpaceSelector::id(space.id());
                return Box::pin(super::space::run(ctx, selector, id, true, None)).await;
            }
            Ok(Started::Elsewhere { reused, place }) => (reused, place),
            Ok(Started::InWrapper) => {
                in_wrappers += 1;
                output.agents.push(ResumedAgent {
                    workspace: space.name().clone(),
                    execution: id.clone(),
                    background: false,
                    reused: true,
                });
                continue;
            }
            Err(error) => {
                let failure = ui::failure(ctx, "resume", space.name(), &error);
                output.failed.push(failure);
                continue;
            }
        };
        place = started.or(place);
        if started.is_none() {
            listed.push((space, id));
        }
        output.agents.push(ResumedAgent {
            workspace: space.name().clone(),
            execution: id.clone(),
            background: started.is_some(),
            reused,
        });
    }
    reports::started(ctx, &output, in_wrappers, place, &listed)?;
    Ok(if output.failed.is_empty() { 0 } else { 1 })
}

/// Where a stopped agent resumes.
enum Started {
    /// Through the wrapper that waits to restore it, which its queued
    /// restore lets go.
    InWrapper,
    /// In this terminal, as the only agent to resume where the backend can
    /// start nothing elsewhere.
    Here,
    /// Where it stopped when `reused`, in the backend's background, or, when
    /// `place` is `None`, in a terminal of the user's own.
    Elsewhere {
        reused: bool,
        place: Option<&'static str>,
    },
}

/// Start the stopped agent `session` of `space` through the wrapper that
/// waits for it, where it stopped or in the background; when none is
/// possible, `single` resumes the only one in this terminal.
async fn start_session(
    ctx: &Context,
    space: &PresentSpace,
    session: &SavedSession,
    single: bool,
) -> Result<Started> {
    if session.waits_in_wrapper {
        queue::queue(ctx, space, &session.id, true).await?;
        return Ok(Started::InWrapper);
    }
    let target = ConfigTarget::Space(SpaceSelector::id(space.id()));
    let settings = crate::config::settings(&ctx.paths, target).await?;
    let args = [
        "resume",
        space.id().as_str(),
        "--execution",
        session.id.as_str(),
        "--focus",
    ]
    .map(OsString::from);
    if let Some(places) =
        terminal::rerun_where_stopped(ctx, &settings, session.origin.as_ref(), &args).await?
    {
        return Ok(Started::Elsewhere {
            reused: true,
            place: Some(places),
        });
    }
    let place = super::places::run_in_background(ctx, &settings, space, &args).await?;
    if single && place.is_none() {
        return Ok(Started::Here);
    }
    Ok(Started::Elsewhere {
        reused: false,
        place,
    })
}
