//! Starting stopped agents at once, each where it stopped, through the
//! wrapper that waits for it, in the background, or in this terminal, and
//! listing those the user starts in terminals of their own.
use anyhow::Result;
use std::{
    ffi::OsString,
    path::{Path, PathBuf},
};

use super::{commands, queue};
use crate::client::execution_ids::ExecutionId;
use crate::client::output::resume::{Resumed, ResumedAgent};
use crate::client::space_names::SpaceSelector;
use crate::{
    cli::{context::Context, terminal, ui},
    execution::recovery::{Saved, waits_in_wrapper},
    model::Workspace,
    protocol::ConfigTarget,
};

/// Resume each workspace's stopped work, reporting it with what `resumed`
/// holds already. Commands without an agent to receive them, as none
/// restoring through its waiting wrapper does, are reported here. An agent
/// resumes where it stopped when the terminal backend can reuse that place,
/// otherwise in the backend's background; without a backend one agent
/// resumes in this terminal unless `resumed` reports chats or failures, and
/// the others are listed for their own terminals.
pub(super) async fn run_all(
    ctx: &Context,
    stopped: Vec<(Workspace, Saved)>,
    mut resumed: Resumed,
) -> Result<i32> {
    let mut sessions = Vec::new();
    for (workspace, mut saved) in stopped {
        if !saved.restores_outside_wrappers() {
            let _claims = commands::claim(&mut saved.commands);
            match commands::report(ctx, &workspace, &saved.commands) {
                Ok(report) => resumed.commands.push(report),
                Err(error) => {
                    let failure = ui::failure(ctx, "resume", &workspace.name, &error);
                    resumed.failed.push(failure);
                }
            }
        }
        sessions.extend(
            saved
                .agents
                .into_iter()
                .chain(saved.sessions)
                .map(|(id, path)| (workspace.clone(), id, path)),
        );
    }
    // An agent restored here replaces this run's report, so it takes the
    // terminal only when there is nothing else to report.
    let alone = resumed.chats.is_empty() && resumed.failed.is_empty();
    let single = sessions.len() == 1 && alone && ctx.interactive();
    start_sessions(ctx, sessions, single, resumed).await
}

/// Start each of the stopped agents `sessions` lists, by workspace,
/// execution and saved file, through the wrapper that waits for it, where it
/// stopped or in the background; `single` resumes the only one in this
/// terminal when none is possible. Reports them with what `output` holds
/// already.
pub(super) async fn start_sessions(
    ctx: &Context,
    sessions: Vec<(Workspace, ExecutionId, PathBuf)>,
    single: bool,
    mut output: Resumed,
) -> Result<i32> {
    let mut listed = Vec::new();
    let mut in_wrappers = 0;
    let mut place = None;
    for (workspace, id, record) in &sessions {
        let (reused, started) = match start_session(ctx, workspace, id, record, single).await {
            Ok(Started::Here) => {
                // Boxed, since `run` calls back into this function.
                let id = Some(id.clone());
                let workspace = SpaceSelector::id(&workspace.id);
                return Box::pin(super::workspace::run(ctx, workspace, id, false, true, None))
                    .await;
            }
            Ok(Started::Elsewhere { reused, place }) => (reused, place),
            Ok(Started::InWrapper) => {
                in_wrappers += 1;
                output.agents.push(ResumedAgent {
                    workspace: workspace.name.clone(),
                    execution: id.clone(),
                    background: false,
                    reused: true,
                });
                continue;
            }
            Err(error) => {
                let failure = ui::failure(ctx, "resume", &workspace.name, &error);
                output.failed.push(failure);
                continue;
            }
        };
        place = started.or(place);
        if started.is_none() {
            listed.push((workspace, id));
        }
        output.agents.push(ResumedAgent {
            workspace: workspace.name.clone(),
            execution: id.clone(),
            background: started.is_some(),
            reused,
        });
    }
    ctx.show(&output, |output| {
        if in_wrappers > 0 {
            let noun = if in_wrappers == 1 { "agent" } else { "agents" };
            outln!("Starting {in_wrappers} {noun} where they wait to restore");
        }
        if let Some(place) = place {
            let started = output.agents.len() - listed.len() - in_wrappers;
            let noun = if started == 1 { "agent" } else { "agents" };
            outln!("Resuming {started} {noun} in {place}");
        }
        if !listed.is_empty() {
            outln!("Resume each agent in its own terminal:");
        }
        for (workspace, id) in &listed {
            let words = [
                "shoal",
                "resume",
                &workspace.name,
                "--execution",
                id.as_str(),
                "--focus",
            ];
            outln!("  {}", crate::shell::quote(&words));
        }
    })?;
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

/// Start the stopped agent `id` of `workspace` through the wrapper that
/// waits for it, where it stopped or in the background; when none is
/// possible, `single` resumes the only one in this terminal.
async fn start_session(
    ctx: &Context,
    workspace: &Workspace,
    id: &ExecutionId,
    record: &Path,
    single: bool,
) -> Result<Started> {
    if waits_in_wrapper(record) {
        queue::queue(ctx, workspace, id, true).await?;
        return Ok(Started::InWrapper);
    }
    let settings = crate::client::workspaces::settings(
        &ctx.paths,
        ConfigTarget::Workspace(SpaceSelector::id(&workspace.id)),
    )
    .await?;
    let args = [
        "resume",
        workspace.id.as_str(),
        "--execution",
        id.as_str(),
        "--focus",
    ]
    .map(OsString::from);
    if let Some(places) = terminal::rerun_where_stopped(ctx, &settings, record, &args).await? {
        return Ok(Started::Elsewhere {
            reused: true,
            place: Some(places),
        });
    }
    let place = terminal::run_in_background(ctx, &settings, workspace, &args).await?;
    if single && place.is_none() {
        return Ok(Started::Here);
    }
    Ok(Started::Elsewhere {
        reused: false,
        place,
    })
}
