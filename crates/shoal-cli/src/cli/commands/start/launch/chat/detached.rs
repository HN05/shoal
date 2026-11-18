//! A new chat that runs detached: the daemon launches it, as it does for
//! an app, in a background place its worker opens.
use anyhow::{Context as _, Result, bail};

use super::{super::super::Options, Started};
use crate::{
    agent::Agent,
    cli::{
        args::create::StartArgs,
        commands::{
            start::{Relaunch, Topic},
            workspaces,
        },
        context::Context,
    },
    model::{QueueEntry, Space},
};
use shoal_protocol::launches::{self, IssueChoice, Launch, Launched, Setup, SpaceChoice};
use shoal_protocol::space_names::SpaceName;

/// The launch the daemon carries out for the chat about `topic` with
/// `options`, run by `agent` under `name`: the chat its `shoal start` or
/// `shoal config setup` makes, in the caller's terminal's host. Arguments
/// for the agent that are not UTF-8 are refused, since the request cannot
/// carry them.
pub(super) fn request(
    topic: &Topic,
    options: &Options,
    (agent, name): (&Agent, &SpaceName),
) -> Result<Launch> {
    let launch = Launch {
        parent: options.parent.clone(),
        name: Some(name.clone()),
        agent: Some(agent.clone()),
        mode: Some(topic.mode.clone()),
        model: options.model.clone(),
        permissions: options.narrowing.value(),
        task: workspaces::launch_args(options.args.clone())?,
        priority: options.priority,
        env: Some(crate::terminal::start_environment()),
        now: options.now,
        ..Default::default()
    };
    Ok(match &topic.relaunch {
        Relaunch::Start(start) => in_place(start, launch),
        Relaunch::ConfigSetup => Launch {
            setup: Some(Setup::Config),
            prompt: (topic.subject.as_ref()).map(|subject| subject.text.clone()),
            ..launch
        },
    })
}

/// `launch` as a chat in the place `shoal start` with `start` names: the
/// repository or workspace it runs in, and the issue it starts on.
fn in_place(start: &StartArgs, launch: Launch) -> Launch {
    let StartArgs {
        repository,
        in_workspace,
        issue,
        ..
    } = start;
    Launch {
        target: repository.clone(),
        in_workspace: in_workspace.clone().flatten().map(SpaceChoice::Space),
        chat: true,
        issue: issue.clone().flatten().map(IssueChoice::Named),
        ..launch
    }
}

/// Have the daemon launch the chat `launch` describes, leaving its report
/// to the caller.
pub(super) async fn start(ctx: &Context, launch: Launch) -> Result<Started> {
    let launched = crate::client::launches::launch(&ctx.paths, launch).await?;
    Ok(Started::Launched(Box::new(launched)))
}

/// Report the chat `launched` started for a start that runs its agent in
/// no terminal, as a start of it in its place would: where its agent runs
/// or waits.
pub(in crate::cli::commands) fn report_started(ctx: &Context, launched: Launched) -> Result<i32> {
    let space = &launched.space;
    let text = match &launched.start {
        launches::Started::Queued { waiting } => {
            let entry = (launched.entry.as_ref())
                .context("the daemon queued the chat's agent without an entry")?;
            queued(space, entry, *waiting)
        }
        launches::Started::Background { place } => place.clone(),
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
        crate::cli::commands::queued_agents::starts_when(entry)
    )
}

#[cfg(test)]
mod tests;
