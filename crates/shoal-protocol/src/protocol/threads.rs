use serde::{Deserialize, Serialize};

use super::{Forwarding, Lifetime, Target};
use crate::code::NewCodeRef;
use crate::item_numbers::ItemNumber;
use crate::space_names::SpaceSelector;
use crate::threads::{CloseCause, NewPost, Resolving, ThreadKind, ThreadNumber, ThreadState};
use shoal_ids::space_ids::SpaceId;

/// Requests about threads: opening one, posting, forwarding, resolving,
/// reading and linking them.
#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ThreadMethod {
    /// Open an empty thread from the caller, a space or the user, with the
    /// space `to` names, else with the caller's coordinator or the user.
    /// Without a kind, it is a `review` thread of the caller's pending
    /// review of the holder, else a question.
    OpenThread {
        name: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        kind: Option<ThreadKind>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        to: Option<SpaceSelector>,
        /// The space the user acts as, as a session in it that Shoal did
        /// not start does; a scoped caller acts as its own.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        from: Option<SpaceSelector>,
        /// Places in code the thread is about, which an opening post,
        /// named as the thread, carries at once.
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        code: Vec<NewCodeRef>,
    },
    /// Open a comment of the caller's pending review of the workspace,
    /// numbered in it on its machine, which keeps the review, for a caller
    /// of another machine; answered with the thread, or `ok` without such
    /// a review.
    OpenComment {
        workspace: SpaceSelector,
        name: String,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        code: Vec<NewCodeRef>,
    },
    /// Close the open threads here that the space `opener` of the
    /// sending machine opened, its comments of the reviews this machine
    /// keeps, and those it holds, for `cause`, as that machine closes its
    /// own; a removal also cancels its pending reviews here. Only another
    /// machine's daemon sends it, as the user.
    CloseOpened { opener: SpaceId, cause: CloseCause },
    /// Reopen the threads here that closing them for `cause` closed, as
    /// `done --undo` and a restore of the space `opener` reopen its own.
    ReopenOpened { opener: SpaceId, cause: CloseCause },
    /// Add a post to the thread from the caller's side.
    Post {
        thread: ThreadNumber,
        post: NewPost,
        /// Files to send with it by absolute path, which the daemon copies.
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        attachments: Vec<crate::files::NewFile>,
        /// Places in code it points at.
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        code: Vec<NewCodeRef>,
        /// Changes it points at, bare numbers of the space it is posted
        /// from.
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        changes: Vec<crate::changes::ChangeNumber>,
        /// The space the user acts as, as a session in it that Shoal did
        /// not start does; a scoped caller acts as its own.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        from: Option<SpaceSelector>,
    },
    /// The caller's agent posts in the thread in its conversation this
    /// turn: the turn's final message becomes the post. Answered with the
    /// mark and the one it replaced.
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
    /// the holder's coordinator or the user, with a note; the caller's
    /// space stays linked to it unless `unlink`.
    ForwardThread {
        thread: ThreadNumber,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        to: Option<SpaceSelector>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        note: Option<String>,
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        unlink: bool,
        /// The space the user acts as, as a session in it that Shoal did
        /// not start does; a scoped caller acts as its own.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        from: Option<SpaceSelector>,
    },
    /// Resolve the thread with `outcome` and its data, or reopen it
    /// without one.
    ResolveThread {
        thread: ThreadNumber,
        #[serde(flatten)]
        resolving: Resolving,
        /// The space the user acts as, as a session in it that Shoal did
        /// not start does; a scoped caller acts as its own.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        from: Option<SpaceSelector>,
    },
    /// The threads the space takes part in, else those the caller may read,
    /// only those whose turn is the user's with `for_user`.
    ListThreads {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        space: Option<SpaceSelector>,
        state: ThreadState,
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        for_user: bool,
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
    /// From now on the changes of the space's agent are for the thread
    /// `thread`, or for none without one.
    SwitchThread {
        space: SpaceSelector,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        thread: Option<ThreadNumber>,
    },
    /// The thread the changes of the space's agent are for now.
    CurrentThread { space: SpaceSelector },
    /// Put the changes `changes` names into the thread, out of any other.
    AddChanges {
        thread: ThreadNumber,
        /// The space whose numbers bare ones name, the caller's own by
        /// default.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        space: Option<SpaceSelector>,
        changes: Vec<crate::changes::ChangeNumber>,
    },
    /// Take the changes `changes` names out of the thread.
    RemoveChanges {
        thread: ThreadNumber,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        space: Option<SpaceSelector>,
        changes: Vec<crate::changes::ChangeNumber>,
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

impl ThreadMethod {
    /// A post goes to the machine the thread is numbered on, waiting for it
    /// while it is offline, and a wait's answer comes as that machine
    /// writes it; what the caller's agent posts in its own conversation is
    /// its hooks' report.
    pub fn forwarding(&self) -> Forwarding {
        match self {
            Self::Post { .. } | Self::CloseOpened { .. } | Self::ReopenOpened { .. } => {
                Forwarding::Queued
            }
            Self::OpenThread { .. }
            | Self::OpenComment { .. }
            | Self::ForwardThread { .. }
            | Self::ResolveThread { .. }
            | Self::ListThreads { .. }
            | Self::ShowThread { .. }
            | Self::LinkThread { .. }
            | Self::UnlinkThread { .. }
            | Self::SwitchThread { .. }
            | Self::CurrentThread { .. }
            | Self::AddChanges { .. }
            | Self::RemoveChanges { .. } => Forwarding::Online,
            Self::AwaitPost { .. } => Forwarding::Streamed,
            Self::PostInChat { .. } | Self::RepliedInChat { .. } => Forwarding::Never,
        }
    }

    pub fn target(&self) -> Option<Target<'_>> {
        match self {
            // A thread a space opens is numbered in its own space, and one
            // the user opens in the space it is opened with.
            Self::OpenThread { from: None, to, .. } => to.as_ref().map(Target::Acting),
            Self::OpenThread { from: Some(_), .. } => None,
            Self::Post { thread, .. }
            | Self::ForwardThread { thread, .. }
            | Self::ResolveThread { thread, .. }
            | Self::ShowThread { thread } => thread.full().map(Target::Item),
            Self::OpenComment { workspace, .. } => Some(Target::Space(workspace)),
            // The machine that keeps them is the one asked.
            Self::CloseOpened { .. } | Self::ReopenOpened { .. } => None,
            Self::PostInChat { space, .. }
            | Self::RepliedInChat { space, .. }
            | Self::LinkThread { space, .. }
            | Self::UnlinkThread { space, .. }
            | Self::AwaitPost { space, .. }
            | Self::SwitchThread { space, .. }
            | Self::CurrentThread { space } => Some(Target::Space(space)),
            Self::ListThreads { space, .. } => space.as_ref().map(Target::Space),
            // Changes are recorded on the machine of their space's worktree.
            Self::AddChanges { space, .. } | Self::RemoveChanges { space, .. } => {
                space.as_ref().map(Target::Space)
            }
        }
    }
}
