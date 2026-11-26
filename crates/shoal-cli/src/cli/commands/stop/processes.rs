//! `shoal stop --processes` and `--leftovers`.
use crate::cli::ui::Acting;
use anyhow::{Result, ensure};

use crate::cli::{context::Context, ui};
use crate::client::output::agents::StoppedProcesses;
use shoal_protocol::processes::{Selection, Stopped};
use shoal_protocol::space_names::SpaceSelector;

pub(in crate::cli::commands) async fn processes(
    ctx: &Context,
    selectors: Vec<SpaceSelector>,
    selection: Selection,
) -> Result<i32> {
    // Unlike what ends a space, a stop of its processes may take the chat it
    // runs in.
    let stopping = stopping_processes(selection);
    let selectors = match ui::current_chat(&None) {
        Some(_) if selectors.is_empty() => {
            let fallback = ui::Fallback::CurrentDirectory(&stopping);
            vec![ui::select_space(ctx, None, fallback).await?]
        }
        _ => ui::select_spaces(ctx, selectors, false, &stopping).await?,
    };
    if selectors.len() > 1 {
        ensure_spaces(ctx, &selectors).await?;
    }
    let mut report = ui::Report::new("stop processes of");
    for selector in selectors {
        match stop_untracked(ctx, &selector, selection).await {
            Ok(stopped) => {
                let message = stopped_message(&stopped, selection);
                report.done(ctx, &message, stopped);
            }
            Err(error) => report.failed(ctx, &selector.to_string(), &error),
        }
    }
    report.finish(ctx, "", |stopped, failed| StoppedProcesses {
        stopped,
        failed,
    })
}

/// Refuse `selectors` before any stops when one names no space, a stopped
/// chat, or a workspace that stopping would refuse now.
async fn ensure_spaces(ctx: &Context, selectors: &[SpaceSelector]) -> Result<()> {
    let spaces = super::resolve(ctx, selectors.to_vec(), false).await?;
    let stopped: Vec<_> = (spaces.iter())
        .filter(|space| space.worktree().is_none() && !space.running)
        .map(|space| space.name.as_str())
        .collect();
    ensure!(
        stopped.is_empty(),
        "{} stopped, so no processes were stopped",
        stopped.join(", ")
    );
    let workspaces: Vec<_> = (spaces.into_iter())
        .filter(|space| space.worktree().is_some())
        .collect();
    super::check_each(ctx, &workspaces).await
}

async fn stop_untracked(
    ctx: &Context,
    space: &SpaceSelector,
    selection: Selection,
) -> Result<Stopped> {
    let requester = Some(std::process::id());
    (crate::client::stopping::stop_processes(space.clone(), selection, requester))
        .send(&ctx.paths)
        .await
}

fn stopped_message(stopped: &Stopped, selection: Selection) -> String {
    let (one, many) = match selection {
        Selection::Processes => ("untracked process", "untracked processes"),
        Selection::Leftovers => ("leftover", "leftovers"),
    };
    match stopped.processes {
        0 => format!("No {many} in {}", stopped.space),
        1 => format!("Stopped 1 {one} in {}", stopped.space),
        n => format!("Stopped {n} {many} in {}", stopped.space),
    }
}

/// The spaces it takes are checked among this machine's.
pub(in crate::cli::commands) fn stopping_processes(selection: Selection) -> Acting {
    Acting::here(move |space| crate::client::stopping::stop_processes(space, selection, None))
}
