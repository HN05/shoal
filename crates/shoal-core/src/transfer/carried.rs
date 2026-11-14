//! The chats a package carries with its space: those in the worktree of the
//! workspace it holds and those below its space, or below one of them, which
//! go where it goes and open again after it.
use crate::client::space_ids::SpaceId;
use crate::model::{Chat, RemovedKind, RemovedSpace};

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
        let sessions = Sessions {
            machine,
            agents: removed.agents.clone(),
            last_sessions: removed.last_sessions.clone(),
        };
        Self {
            remote_url,
            chat: super::moved(removed),
            sessions,
        }
    }

    fn chat_with(&self, sessions: Sessions) -> RemovedSpace {
        let session = sessions
            .reported()
            .next()
            .map(|(_, session)| session.clone());
        let mut chat = RemovedSpace {
            agents: sessions.agents,
            last_sessions: sessions.last_sessions,
            ..self.chat.clone()
        };
        if let RemovedKind::Chat { chat } = &mut chat.kind {
            chat.session = session;
        }
        chat
    }
}

#[cfg(test)]
mod tests;
