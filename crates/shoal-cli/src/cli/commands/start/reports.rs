//! What a start reports of the agent it started: the machine the hub
//! placed it on, where a chat's agent runs or waits when it runs in no
//! terminal, and where an agent waits in the queue.
use anyhow::{Context as _, Result, bail};

use crate::{
    cli::context::Context,
    model::{QueueEntry, Space},
    state::QueueState,
};
use shoal_protocol::hub::placement::{Decision, Unanswered};
use shoal_protocol::protocol::{self as launches, Launched};
use shoal_schedule::placement::{Reason, SkippedBy};

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
        crate::client::output::queue::starts_when(entry)
    )
}

/// Report the agent of `entry`, queued in the space `name`.
pub(in crate::cli::commands) fn report_queued_in(
    ctx: &Context,
    name: &str,
    entry: &QueueEntry,
) -> Result<i32> {
    ctx.emit(&crate::client::output::queue::queued_in(name, entry), entry)?;
    Ok(0)
}

/// Report the machine the hub placed a start on, `decision`, and why, with
/// each machine it skipped.
pub(in crate::cli::commands) fn report_placement(ctx: &Context, decision: &Decision) -> Result<()> {
    let text = std::iter::once(placed(decision))
        .chain(decision.skipped.iter().map(|skipped| {
            format!(
                "Skipped machine {}: {}",
                skipped.name,
                skipped_why(skipped.why)
            )
        }))
        .collect::<Vec<_>>()
        .join("\n");
    ctx.emit(&text, decision)
}

/// What a start says of the machine `decision` placed it on.
fn placed(decision: &Decision) -> String {
    let unanswered =
        (decision.unanswered).map(|unanswered| format!(", as {}", unanswered_why(unanswered)));
    format!(
        "Placed on machine {} by {}{}: {}",
        decision.name,
        decision.policy,
        unanswered.unwrap_or_default(),
        reason(decision.reason)
    )
}

/// Why a placement put a start on a machine.
fn reason(reason: Reason) -> String {
    let memory = |memory: Option<u64>| {
        memory.map_or_else(String::new, |memory| {
            format!(", {} memory free", crate::cli::ui::size_label(memory))
        })
    };
    match reason {
        Reason::Local => "placement is local".into(),
        Reason::HeavySlot { memory_free } => {
            format!("always_on, heavy slot free{}", memory(memory_free))
        }
        Reason::NoHeavyAgents => "always_on, runs no heavy agents".into(),
        Reason::NoHeavyWaiting { memory_free } => {
            format!("always_on, no heavy agents waiting{}", memory(memory_free))
        }
        Reason::HeavyWaiting { waiting } => format!("always_on, {waiting} heavy agents waiting"),
        Reason::Caller => "no always_on machine has room".into(),
        Reason::Intermittent => "intermittent".into(),
        Reason::Full => "always_on, no room".into(),
        Reason::Unreported => "capacity not reported yet".into(),
    }
}

/// Why the external placer did not decide.
fn unanswered_why(unanswered: Unanswered) -> &'static str {
    match unanswered {
        Unanswered::NoPlacer => "no placer is registered",
        Unanswered::Late => "the placer did not answer in time",
        Unanswered::Failed => "the placer chose no connected machine",
    }
}

/// Why a placement skipped a machine, with what lets a start go there.
fn skipped_why(why: SkippedBy) -> &'static str {
    match why {
        SkippedBy::NoRepository => "repository not registered; `shoal repo add` there registers it",
        SkippedBy::NoAgent => "agent tool not installed",
    }
}
