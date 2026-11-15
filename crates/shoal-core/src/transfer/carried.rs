//! The chats a package carries with its space: those in the worktree of the
//! workspace it holds and those below its space, or below one of them, which
//! go where it goes and open again after it.
use crate::model::{Chat, RemovedSpace};
use shoal_ids::space_ids::SpaceId;

use super::{CarriedChat, Sessions};

/// The IDs of the `chats` that go with the space `space`, each after its
/// parent among them: those in its worktree and those below it or below one
/// of them. A chat below a workspace below it goes with that workspace.
pub fn carried_chats(chats: &[Chat], space: &SpaceId) -> Vec<SpaceId> {
    let goes = |chat: &Chat, carried: &[SpaceId]| {
        let in_worktree = (chat.workspace.as_ref()).is_some_and(|workspace| &workspace.id == space);
        let below = (chat.parent.as_ref())
            .is_some_and(|parent| &parent.id == space || carried.contains(&parent.id));
        in_worktree || below
    };
    let mut carried: Vec<SpaceId> = Vec::new();
    while let Some(chat) =
        (chats.iter()).find(|chat| !carried.contains(&chat.id) && goes(chat, &carried))
    {
        carried.push(chat.id.clone());
    }
    let parent = |id: &SpaceId| {
        let chat = chats.iter().find(|chat| &chat.id == id)?;
        chat.parent.as_ref().map(|parent| &parent.id)
    };
    let mut ordered: Vec<SpaceId> = Vec::new();
    while let Some(next) = carried.iter().find(|id| {
        let waits =
            parent(id).is_some_and(|parent| carried.contains(parent) && !ordered.contains(parent));
        !ordered.contains(id) && !waits
    }) {
        ordered.push(next.clone());
    }
    ordered
}

/// Operations on the client type [`CarriedChat`] that only the `shoal`
/// command uses.
pub trait CarriedChatExt: Sized {
    /// The chat `removed` records, of the repository with the origin
    /// `remote_url`, whose agent's sessions `machine` recorded, carried to
    /// another machine.
    fn of(remote_url: Option<String>, machine: Option<String>, removed: RemovedSpace) -> Self;

    /// The removal record to import, whose agent has `sessions`.
    fn chat_with(&self, sessions: Sessions) -> RemovedSpace;
}

impl CarriedChatExt for CarriedChat {
    fn of(remote_url: Option<String>, machine: Option<String>, removed: RemovedSpace) -> Self {
        Self {
            remote_url,
            sessions: super::recorded_sessions(machine, &removed),
            chat: super::moved(removed),
        }
    }

    fn chat_with(&self, sessions: Sessions) -> RemovedSpace {
        super::with_sessions(&self.chat, sessions)
    }
}

#[cfg(test)]
mod tests;
