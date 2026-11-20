//! Starting stopped agents at once, each where it stopped, through the
//! wrapper that waits for it, in the background, or in this terminal, and
//! listing those the user starts in terminals of their own.
use anyhow::Result;

use super::reports::{self, Reporting};
use crate::client::output::resume::StartedRestores;
use crate::{
    cli::{context::Context, ui},
    execution::recovery::{Saved, SavedSession},
    model::Space,
};
use shoal_protocol::space_names::SpaceSelector;
use shoal_workers::resume::{
    commands,
    sessions::{self, Starts},
};

/// Resume each space's stopped work, reporting it with what `resumed` holds
/// already. Commands without an agent to receive them, as none restoring
/// through its waiting wrapper does, are reported here. An agent resumes
/// where it stopped when the terminal backend can reuse that place,
/// otherwise in the backend's background; without a backend one agent
/// resumes in this terminal unless `resumed` reports continued
/// turns or failures, and the others are listed for their own terminals.
pub(super) async fn run_all(
    ctx: &Context,
    stopped: Vec<(Space, Saved)>,
    mut resumed: StartedRestores,
) -> Result<i32> {
    let mut sessions = Vec::new();
    for (space, mut saved) in stopped {
        if !saved.restores_outside_wrappers() {
            match commands::report_claimed(&Reporting(ctx), &space, &mut saved.commands) {
                Ok(report) => resumed.commands.push(report),
                Err(error) => {
                    let failure = ui::failure(ctx, "resume", &space.name, &error);
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
    let alone = resumed.continued.is_empty() && resumed.failed.is_empty();
    let single = sessions.len() == 1 && alone && ctx.on_terminal();
    start_sessions(ctx, sessions, single, resumed).await
}

/// Start each of the stopped agents `sessions` lists, by space, through the
/// wrapper that waits for it, where it stopped or in the background;
/// `single` resumes the only one in this terminal when none is possible.
/// Reports them with what `output` holds already.
pub(super) async fn start_sessions(
    ctx: &Context,
    sessions: Vec<(Space, SavedSession)>,
    single: bool,
    output: StartedRestores,
) -> Result<i32> {
    let reporting = Reporting(ctx);
    let launcher = ctx.launcher();
    let starts = sessions::start(&ctx.paths, launcher, sessions, single, output, &reporting);
    report_starts(ctx, starts.await?).await
}

/// Report the agents `starts` started, or restore the one it leaves to this
/// terminal here.
pub(super) async fn report_starts(ctx: &Context, starts: Starts) -> Result<i32> {
    match starts {
        Starts::Here { space, execution } => {
            let selector = SpaceSelector::id(&space);
            super::space::here(ctx, selector, Some(execution), None).await
        }
        Starts::Elsewhere(started) => {
            reports::started(ctx, &started)?;
            let failed = &started.output.failed;
            Ok(if failed.is_empty() { 0 } else { 1 })
        }
    }
}
