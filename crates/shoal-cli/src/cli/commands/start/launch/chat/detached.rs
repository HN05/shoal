//! A new chat that runs detached: the daemon launches it, as it does for
//! an app, in a background place its worker opens.
use anyhow::Result;

use super::{super::super::Options, Started};
use crate::{
    agent::Agent,
    cli::{
        args::create::StartArgs,
        commands::start::{Narrowing, Relaunch, Topic},
        context::Context,
    },
};
use shoal_protocol::chats::{QueuedChat, StartedChat};
use shoal_protocol::launches::{IssueChoice, Launch, Launched, SpaceChoice};
use shoal_protocol::space_names::SpaceName;

/// The launch the daemon carries out for the chat about `topic` with
/// `options`, run by `agent` under `name`: the chat its `shoal start` makes,
/// in the caller's terminal's host. `None` for one only this process can
/// start, since the request cannot carry it: a restore, a session the chat
/// continues or forks, an imported chat's links, `shoal config setup`, or
/// arguments that are not UTF-8.
pub(super) fn request(
    topic: &Topic,
    options: &Options,
    (agent, name): (&Agent, &SpaceName),
) -> Option<Launch> {
    let Relaunch::Start(start) = &topic.relaunch else {
        return None;
    };
    let StartArgs {
        repository,
        in_workspace,
        issue,
        ..
    } = &**start;
    let Narrowing::Requested(narrowing) = options.narrowing else {
        return None;
    };
    let carried = options.session.is_none() && options.handoff.is_none() && !topic.tells_resumed;
    if !carried {
        return None;
    }
    let task = (options.args.iter())
        .map(|arg| arg.to_str().map(str::to_owned))
        .collect::<Option<_>>()?;
    Some(Launch {
        target: repository.clone(),
        in_workspace: in_workspace.clone().flatten().map(SpaceChoice::Space),
        chat: true,
        parent: options.parent.clone(),
        name: Some(name.clone()),
        agent: Some(agent.clone()),
        mode: Some(topic.mode.clone()),
        model: options.model.clone(),
        permissions: narrowing,
        issue: issue.clone().flatten().map(IssueChoice::Named),
        task,
        priority: options.priority,
        env: Some(crate::terminal::start_environment()),
        now: options.now,
        ..Default::default()
    })
}

/// Have the daemon launch the chat `launch` describes, reporting one that
/// runs in a background place to the caller, one an app runs here, and one
/// whose agent waits in the agent queue.
pub(super) async fn start(ctx: &Context, launch: Launch) -> Result<Started> {
    // A launch that asks for a chat starts in one.
    let Launched::Chat(started) = crate::client::launches::launch(&ctx.paths, launch).await? else {
        anyhow::bail!("the daemon started the chat's agent in no chat");
    };
    reported(ctx, started)
}

/// Report the chat the daemon started for a start that runs it in no
/// terminal, as a start of it in its place would.
pub(in crate::cli::commands) fn report_started(ctx: &Context, started: StartedChat) -> Result<i32> {
    match reported(ctx, started)? {
        Started::Exited(code) => Ok(code),
        Started::Background(chat) => {
            ctx.emit(&chat.place, &chat)?;
            Ok(0)
        }
    }
}

/// How the daemon `started` the chat, once what only the start reports is.
fn reported(ctx: &Context, started: StartedChat) -> Result<Started> {
    match started {
        StartedChat::Queued(queued) => {
            report_queued(ctx, &queued)?;
            Ok(Started::Exited(0))
        }
        StartedChat::Background(chat) => Ok(Started::Background(chat)),
        StartedChat::Detached(report) => {
            let text = format!(
                "Started chat {}; its output goes to {}",
                report.chat.name,
                report.log.display()
            );
            ctx.emit(&text, &*report)?;
            Ok(Started::Exited(0))
        }
    }
}

/// Report the recorded chat of `queued`, whose agent waits in the queue.
pub(super) fn report_queued(ctx: &Context, queued: &QueuedChat) -> Result<()> {
    let entry = &queued.entry;
    let text = format!(
        "Queued chat {} (position {} of {}); its agent starts {}",
        queued.chat.name,
        entry.position.unwrap_or(queued.waiting),
        queued.waiting,
        crate::cli::commands::queued_agents::starts_when(entry)
    );
    ctx.emit(&text, queued)
}

#[cfg(test)]
mod tests;
