//! Stopping the processes Shoal does not track, which are not saved for
//! `shoal resume`.
use anyhow::{Result, ensure};

use crate::client::output::agents::StoppedProcesses;
use crate::client::space_names::SpaceSelector;
use crate::{
    cli::{context::Context, ui},
    client::processes::{Selection, Stopped},
    model::PresentSpace,
};

/// Stop the processes Shoal does not track that `selection` takes, of each
/// space `selectors` names, else of the current chat or workspace, else of
/// one picked. They are not saved for `shoal resume`.
pub(in crate::cli::commands) async fn processes(
    ctx: &Context,
    selectors: Vec<SpaceSelector>,
    selection: Selection,
) -> Result<i32> {
    // Unlike what ends a space, a stop of its processes may take the chat it
    // runs in.
    let selectors = match ui::current_chat(&None) {
        Some(_) if selectors.is_empty() => {
            vec![ui::select_space(ctx, None, ui::Fallback::CurrentDirectory).await?]
        }
        _ => ui::select_spaces(ctx, selectors, false).await?,
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
        .filter_map(|space| match space {
            PresentSpace::Chat(chat) if chat.restore.is_some() => Some(chat.name.as_str()),
            _ => None,
        })
        .collect();
    ensure!(
        stopped.is_empty(),
        "{} stopped, so no processes were stopped",
        stopped.join(", ")
    );
    let workspaces: Vec<_> = (spaces.into_iter())
        .filter(|space| matches!(space, PresentSpace::Workspace(_)))
        .collect();
    super::check_each(ctx, &workspaces).await
}

/// Stop the untracked processes of `space` that `selection` takes, other
/// than this one and the processes that started it.
pub(super) async fn stop_untracked(
    ctx: &Context,
    space: &SpaceSelector,
    selection: Selection,
) -> Result<Stopped> {
    let requester = Some(std::process::id());
    crate::client::stopping::stop_processes(&ctx.paths, space.clone(), selection, requester).await
}

/// What a stop of `selection` reports for `stopped`.
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
