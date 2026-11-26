//! What a start reports of the agent it started.
use anyhow::{Context as _, Result, bail};

use crate::{
    cli::context::Context,
    model::{QueueEntry, Space},
    state::QueueState,
};
use shoal_protocol::hub::placement::{Decision, Unanswered};
use shoal_protocol::protocol::{self as launches, Launched};
use shoal_schedule::placement::{Reason, SkippedBy};

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

fn queued(space: &Space, entry: &QueueEntry, waiting: usize) -> String {
    format!(
        "Queued chat {}, {} (position {} of {waiting}); its agent starts {}",
        space.number,
        space.name,
        entry.position.unwrap_or(waiting),
        crate::client::output::queue::starts_when(entry)
    )
}

pub(in crate::cli::commands) fn report_queued_in(
    ctx: &Context,
    name: &str,
    entry: &QueueEntry,
) -> Result<i32> {
    ctx.emit(&crate::client::output::queue::queued_in(name, entry), entry)?;
    Ok(0)
}

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
    }
}

fn unanswered_why(unanswered: Unanswered) -> &'static str {
    match unanswered {
        Unanswered::NoPlacer => "no placer is registered",
        Unanswered::Late => "the placer did not answer in time",
        Unanswered::Failed => "the placer chose no machine the start can go to",
    }
}

fn skipped_why(why: SkippedBy) -> &'static str {
    match why {
        SkippedBy::NoRepository => "repository not registered; `shoal repo add` there registers it",
        SkippedBy::NoAgent => "agent tool not installed",
        SkippedBy::Away => "its user is away; it starts nothing new until they return",
        SkippedBy::Paused => "paused; it starts nothing new until `shoal unpause` there",
    }
}
