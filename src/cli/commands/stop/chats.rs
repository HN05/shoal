//! Stopping a chat: the daemon saves it for `shoal resume` and stops the
//! processes it owns, and a chat stopped already stays as it is.
use anyhow::Result;

use crate::client::output::agents::SpaceStopped;
use crate::client::space_names::{SpaceName, SpaceSelector};
use crate::{
    cli::{context::Context, output::Style},
    model::{Chat, StoppedChat},
};

use super::continue_hint;

/// Stop `chat`, reporting it alone.
pub(super) async fn stop_one(ctx: &Context, chat: &Chat) -> Result<i32> {
    let stopped = stop(ctx, chat).await?;
    let mut message = stopped_message(&chat.name, stopped);
    if !stopped.already_stopped {
        message = format!("{message}. {}", continue_hint(&chat.name));
    }
    ctx.emit_styled(
        Style::Success,
        &message,
        SpaceStopped {
            stopped: true,
            saved: true,
            already_stopped: stopped.already_stopped,
            other_processes: 0,
            cancelled: Vec::new(),
            cancelled_run: None,
        },
    )?;
    Ok(0)
}

/// Save `chat` for `shoal resume` and stop its agent.
pub(super) async fn stop(ctx: &Context, chat: &Chat) -> Result<StoppedChat> {
    crate::client::stopping::stop_chat(&ctx.paths, SpaceSelector::id(&chat.id)).await
}

/// What a stop reports for the chat `name` it `stopped`.
pub(super) fn stopped_message(name: &SpaceName, stopped: StoppedChat) -> String {
    match stopped.already_stopped {
        true => format!("{name} was already stopped"),
        false => format!("Stopped {name}"),
    }
}
