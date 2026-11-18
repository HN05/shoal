//! An admitted launch in a chat: the chat starts in a background place
//! where the terminal that queued it would have opened one, and the chat's
//! agent takes the launch's slot.
use anyhow::Result;

use crate::{
    client::connection::CommandPaths,
    model::Chat,
    protocol::{ChatStart, operations::QueuedLaunch},
};
use shoal_ids::queue_entry_ids::QueueEntryId;
use shoal_protocol::space_names::SpaceSelector;

/// Start `launch`, the agent of the admitted launch `entry`, in the
/// recorded `chat`, its agent taking the launch's slot.
pub(super) async fn start(
    paths: &CommandPaths,
    entry: &QueueEntryId,
    chat: &Chat,
    launch: QueuedLaunch,
) -> Result<()> {
    crate::env::launch_queue_entry(entry);
    crate::chat_starts::start_queued(paths, chat_start(chat, launch), chat.id.clone()).await
}

/// The start of `chat`'s agent as `launch` asks for it, where the chat
/// runs.
fn chat_start(chat: &Chat, launch: QueuedLaunch) -> ChatStart {
    let QueuedLaunch {
        agent,
        prompt,
        mode,
        narrowing,
        model,
        args,
        issue,
        now,
        ..
    } = launch;
    let workspace = chat.workdir.workspace().map(|workspace| &workspace.id);
    ChatStart {
        repository: (chat.repository_id.as_ref())
            .filter(|_| workspace.is_none())
            .map(Into::into),
        workspace: workspace.map(SpaceSelector::id),
        name: Some(chat.name.clone()),
        agent: Some(agent),
        mode,
        model,
        narrowing,
        issue: issue.map(|issue| issue.url.into()),
        prompt,
        args,
        now,
        ..ChatStart::default()
    }
}
