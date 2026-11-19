//! What a start reports of the agent it started: where a chat's agent
//! runs or waits when it runs in no terminal, and where an agent waits in
//! the queue.
use anyhow::{Context as _, Result, bail};

use crate::{
    cli::{commands::queued_agents, context::Context},
    model::{QueueEntry, Space},
    state::QueueState,
};
use shoal_protocol::protocol::{self as launches, Launched};

/// Report the chat `launched` started for a start that runs its agent in
/// no terminal, as a start of it in its place would: where its agent runs
/// or waits.
pub(in crate::cli::commands) fn report_started(ctx: &Context, launched: Launched) -> Result<i32> {
    let space = &launched.space;
    let text = match &launched.start {
        launches::Started::Queued { waiting } => {
            let entry = (launched.entry.as_ref())
                .context("the daemon queued the chat's agent without an entry")?;
            match entry.state {
                QueueState::Queued => queued(space, entry, *waiting),
                QueueState::Starting | QueueState::Failed => {
                    format!("Starting chat {}, {}", space.number, space.name)
                }
            }
        }
        launches::Started::Detached(run) => format!(
            "Started chat {}; its output goes to {}",
            space.name,
            run.log.display()
        ),
        launches::Started::Preparing { .. } | launches::Started::Worker => {
            bail!("the daemon started the chat's agent in no chat")
        }
    };
    ctx.emit(&text, &launched)?;
    Ok(0)
}

/// What a start says of the chat `space`, whose agent waits in the queue as
/// `entry` behind `waiting` entries.
fn queued(space: &Space, entry: &QueueEntry, waiting: usize) -> String {
    format!(
        "Queued chat {}, {} (position {} of {waiting}); its agent starts {}",
        space.number,
        space.name,
        entry.position.unwrap_or(waiting),
        queued_agents::starts_when(entry)
    )
}

/// Report the agent of `entry`, queued in the space `name`.
pub(in crate::cli::commands) fn report_queued_in(
    ctx: &Context,
    name: &str,
    entry: &QueueEntry,
) -> Result<i32> {
    let text = match entry.state {
        QueueState::Queued => format!(
            "Queued {} in {name} (position {}); it starts {}",
            entry.agent,
            entry.position.unwrap_or(1),
            queued_agents::starts_when(entry)
        ),
        _ => format!("Starting {} in {name}", entry.agent),
    };
    ctx.emit(&text, entry)?;
    Ok(0)
}
