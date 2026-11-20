use serde::{Deserialize, Serialize};

use super::Lifetime;
use crate::item_numbers::ItemNumber;
use crate::space_names::SpaceSelector;
use crate::threads::{NewPost, OutcomeKind, ThreadKind, ThreadNumber, ThreadState};

/// Requests about threads: opening one, posting, forwarding, resolving,
/// reading and linking them.
#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ThreadMethod {
    /// Open an empty thread from the caller, a space or the user, with the
    /// space `to` names, else with the caller's coordinator or the user.
    OpenThread {
        name: String,
        kind: ThreadKind,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        to: Option<SpaceSelector>,
        /// The space the user acts as, as a session in it that Shoal did
        /// not start does; a scoped caller acts as its own.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        from: Option<SpaceSelector>,
    },
    /// Add a post to the thread from the caller's side.
    Post {
        thread: ThreadNumber,
        post: NewPost,
        /// Files to send with it by absolute path, which the daemon copies.
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        attachments: Vec<crate::files::NewFile>,
        /// The space the user acts as, as a session in it that Shoal did
        /// not start does; a scoped caller acts as its own.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        from: Option<SpaceSelector>,
    },
    /// The caller's agent posts in the thread in its conversation this
    /// turn: the turn's final message becomes the post.
    PostInChat {
        space: SpaceSelector,
        thread: ThreadNumber,
    },
    /// The user replied in the conversation of the caller's agent with
    /// `prompt`, which answers the posts its agent made there.
    RepliedInChat {
        space: SpaceSelector,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        prompt: Option<String>,
    },
    /// Give the thread the caller holds to the space `to` names, else to
    /// the holder's coordinator or the user, with a note.
    ForwardThread {
        thread: ThreadNumber,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        to: Option<SpaceSelector>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        note: Option<String>,
        /// The space the user acts as, as a session in it that Shoal did
        /// not start does; a scoped caller acts as its own.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        from: Option<SpaceSelector>,
    },
    /// Resolve the thread with `outcome` and its data, or reopen it
    /// without one.
    ResolveThread {
        thread: ThreadNumber,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        outcome: Option<OutcomeKind>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        choice: Option<u32>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        of: Option<ThreadNumber>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        reason: Option<String>,
        /// The space the user acts as, as a session in it that Shoal did
        /// not start does; a scoped caller acts as its own.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        from: Option<SpaceSelector>,
    },
    /// The threads the space takes part in, else those the caller may read.
    ListThreads {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        space: Option<SpaceSelector>,
        state: ThreadState,
    },
    /// One thread by number.
    ShowThread { thread: ThreadNumber },
    /// Deliver the thread's new posts to the space's agent.
    LinkThread {
        space: SpaceSelector,
        thread: ItemNumber,
    },
    /// Stop delivering them.
    UnlinkThread {
        space: SpaceSelector,
        thread: ItemNumber,
    },
    /// Long-lived: the thread `item` of `space` once the other side of the
    /// post `after` posts, or it is resolved.
    AwaitPost {
        space: SpaceSelector,
        item: u32,
        after: i64,
    },
}

impl ThreadMethod {
    pub fn lifetime(&self) -> Lifetime {
        match self {
            Self::AwaitPost { .. } => Lifetime::Resumed,
            _ => Lifetime::Reply,
        }
    }
}
