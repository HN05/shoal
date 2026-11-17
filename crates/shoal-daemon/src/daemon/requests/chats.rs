//! The daemon's answers to [`ChatMethod`] requests, and who may send them.
use std::sync::Arc;

use anyhow::Result;

use super::super::{
    answer::Answer,
    chats::NewChat,
    scope::{Caller, Scoped},
    workspace::Manager,
};
use crate::protocol::{Body, ChatMethod, ChatStart};
use shoal_ids::queue_entry_ids::QueueEntryId;
use shoal_protocol::chats::LaunchedChat;
use shoal_protocol::space_names::SpaceSelector;

pub(super) async fn handle(
    manager: &Arc<Manager>,
    method: ChatMethod,
    caller: Option<&Caller>,
) -> Result<Answer> {
    Ok(Answer::Body(match method {
        ChatMethod::LaunchChat {
            start,
            chat,
            origin,
            queue_entry,
        } => {
            let launched = Launched {
                chat,
                origin,
                entry: queue_entry,
            };
            Body::LaunchedChat(launched.take(manager, *start, caller).await?)
        }
        ChatMethod::CheckChatParent {
            repository,
            workspace,
            parent,
        } => {
            if caller.is_none() {
                (manager.ensure_parent_not_host(workspace.as_ref(), parent.as_ref())).await?;
            }
            let parent = manager.space_id(parent.as_ref()).await?;
            (manager.ensure_parent_takes(repository.as_ref(), parent.as_ref())).await?;
            Body::Ok
        }
        ChatMethod::ChatRoot => Body::ChatRoot(crate::protocol::ChatRoot {
            path: manager.prepare_chat_root().await?,
        }),
    }))
}

/// What a caller that runs a chat's agent itself names of its start: the
/// chat the daemon recorded for it, the place it runs in and the launch
/// whose slot its agent takes.
struct Launched {
    chat: Option<SpaceSelector>,
    origin: Option<crate::sessions::Origin>,
    entry: Option<QueueEntryId>,
}

impl Launched {
    /// The chat whose agent `caller` runs: the one the daemon recorded for
    /// its start, placed at its origin, which a scoped caller's scope must
    /// reach, else a new one `start` describes, recorded with its launch
    /// attached and linked.
    async fn take(
        self,
        manager: &Manager,
        start: ChatStart,
        caller: Option<&Caller>,
    ) -> Result<LaunchedChat> {
        let recorded = manager.recorded_launch(self.chat.as_ref(), self.entry.as_ref());
        let Some(chat) = recorded.await? else {
            return self.record(manager, start, caller).await;
        };
        if let Some(caller) = caller {
            let space = manager.space(&SpaceSelector::id(&chat)).await?;
            manager.ensure_reaches(caller, &space).await?;
        }
        manager.take_recorded_chat(&chat, self.origin).await
    }

    /// Record the new chat `start` describes for `caller`, confined as the
    /// scoped process that queued its launch would be, with the
    /// permissions and priority it may give.
    async fn record(
        self,
        manager: &Manager,
        mut start: ChatStart,
        caller: Option<&Caller>,
    ) -> Result<LaunchedChat> {
        if caller.is_none() {
            let (workspace, parent) = (start.workspace.as_ref(), start.parent.as_ref());
            manager.ensure_parent_not_host(workspace, parent).await?;
        }
        let entry = self.entry.as_ref();
        let queued = queued_scope(manager, caller, entry, &mut start.parent).await?;
        let caller = caller.or(queued.as_ref());
        let held = (start.repository.as_ref(), start.workspace.as_ref());
        (manager.ensure_chat_given_held(caller, &start.narrowing, held)).await?;
        start.priority = manager.given_priority(caller, start.priority).await?;
        let parent = manager.space_id(start.parent.as_ref()).await?;
        let new = NewChat::of(&start, parent, self.origin)?;
        manager.record_launch(&start, new, entry, caller).await
    }
}

/// The scope a chat the user's process records for the launch `entry` is
/// confined to: that of the space whose scoped process queued the launch,
/// below which `parent` then goes, as that process's own start would put
/// it; none for a scoped `caller`, whose own scope confines it, or for a
/// launch the user queued.
async fn queued_scope(
    manager: &Manager,
    caller: Option<&Caller>,
    entry: Option<&shoal_ids::queue_entry_ids::QueueEntryId>,
    parent: &mut Option<SpaceSelector>,
) -> Result<Option<Caller>> {
    let (None, Some(entry)) = (caller, entry) else {
        return Ok(None);
    };
    let Some(queued) = manager.queued_scope(entry).await? else {
        return Ok(None);
    };
    manager.parent_below(&queued, parent).await?;
    Ok(Some(queued))
}

impl Scoped for ChatMethod {
    async fn scope(
        &mut self,
        manager: &Manager,
        caller: &Caller,
    ) -> Result<Option<&mut SpaceSelector>> {
        Ok(match self {
            // A scoped caller starts chats only in its own tree, below its
            // own space unless it names one below it, in a worktree of its
            // tree, and runs each in the session it starts, with its scope.
            // A named parent is never the workspace the chat runs in; one
            // it leaves out is its own space, wherever the chat runs.
            // A chat the daemon recorded for its start is one below it.
            ChatMethod::LaunchChat {
                chat: Some(chat), ..
            } => Some(
                manager
                    .workspace_below(
                        caller,
                        chat,
                        "scoped processes run only the chats below their own space",
                    )
                    .await?,
            ),
            ChatMethod::LaunchChat { start, .. } => {
                let workspace = start.workspace.as_ref();
                (manager.ensure_parent_not_host(workspace, start.parent.as_ref())).await?;
                manager.parent_below(caller, &mut start.parent).await?;
                start.workspace.as_mut()
            }
            // Its check takes the parent as its start would.
            ChatMethod::CheckChatParent {
                parent, workspace, ..
            } => {
                (manager.ensure_parent_not_host(workspace.as_ref(), parent.as_ref())).await?;
                manager.parent_below(caller, parent).await?;
                None
            }
            ChatMethod::ChatRoot => None,
        })
    }
}

#[cfg(test)]
mod tests;
