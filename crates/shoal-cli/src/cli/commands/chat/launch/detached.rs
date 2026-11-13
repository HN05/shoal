//! A new chat that runs detached: the daemon starts it through `LaunchChat`,
//! as it does for an app, in a background place its worker opens.
use anyhow::Result;

use super::{super::Options, Started};
use crate::client::space_names::SpaceName;
use crate::{
    agent::Agent,
    cli::{
        args::{create::StartArgs, target::StartTarget},
        commands::chat::{Narrowing, Relaunch, Topic},
        context::Context,
    },
    client::output::chat::StartedChat,
    protocol::ChatStart,
};

/// The start the daemon carries out for the chat about `topic` with
/// `options`, run by `agent` under `name`: the chat its `shoal start` makes,
/// in the caller's terminal's host. `None` for one only this process can
/// start, since the request cannot carry it: a restore, a session the chat
/// continues or forks, an imported chat's links, `shoal config setup`, or
/// arguments that are not UTF-8.
pub(super) fn request(
    topic: &Topic,
    options: &Options,
    (agent, name): (&Agent, &SpaceName),
) -> Option<ChatStart> {
    let Relaunch::Start(start) = &topic.relaunch else {
        return None;
    };
    let StartArgs {
        repository,
        in_workspace,
        issue,
        linked_issue,
        ..
    } = &**start;
    let Narrowing::Requested(narrowing) = options.narrowing else {
        return None;
    };
    let carried = options.session.is_none()
        && options.handoff.is_none()
        && linked_issue.is_none()
        && !topic.tells_resumed;
    if !carried {
        return None;
    }
    let repository = match repository {
        Some(StartTarget::Repository(id)) => Some(id.as_str().into()),
        _ => None,
    };
    let args = (options.args.iter())
        .map(|arg| arg.to_str().map(str::to_owned))
        .collect::<Option<_>>()?;
    Some(ChatStart {
        repository,
        workspace: in_workspace.clone().flatten(),
        parent: options.parent.clone(),
        name: Some(name.clone()),
        agent: Some(agent.clone()),
        mode: Some(topic.mode.clone()),
        model: options.model.clone(),
        narrowing,
        issue: issue.clone().flatten(),
        args,
        priority: options.priority,
        env: Some(crate::terminal::start_environment()),
    })
}

/// Have the daemon start the chat `start` describes, reporting one that
/// runs in a background place to the caller and one an app runs here.
pub(super) async fn start(ctx: &Context, start: ChatStart) -> Result<Started> {
    match crate::client::chats::launch(&ctx.paths, start).await? {
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

#[cfg(test)]
mod tests;
