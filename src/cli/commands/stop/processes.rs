//! Stopping the processes Shoal does not track, which are not saved for
//! `shoal resume`.
use anyhow::Result;

use crate::client::output::agents::StoppedProcesses;
use crate::client::space_names::SpaceSelector;
use crate::{
    cli::{context::Context, ui},
    client::processes::{Selection, Stopped},
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

/// Refuse `selectors` before any stops when one names neither a running
/// chat nor a workspace that stopping accepts.
async fn ensure_spaces(ctx: &Context, selectors: &[SpaceSelector]) -> Result<()> {
    let chats = crate::client::chats::list(&ctx.paths).await?;
    let workspaces: Vec<SpaceSelector> = selectors
        .iter()
        .filter(|selector| {
            !(chats.iter()).any(|chat| selector.selects(&chat.id, &chat.name, chat.number))
        })
        .cloned()
        .collect();
    super::inspect_each(ctx, &workspaces).await?;
    Ok(())
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
