//! Threads: conversations between whoever opened one and whoever holds it.
use anyhow::Result;
use shoal_protocol::{
    chat_marks::ChatMarked,
    code::NewCodeRef,
    files::NewFile,
    item_numbers::ItemNumber,
    protocol::ThreadMethod,
    space_names::SpaceSelector,
    threads::{Listing, NewPost, Resolving, Thread, ThreadKind, ThreadNumber, ThreadState},
};

use crate::connection::{Daemon, await_reply, request};

/// Open a thread `name` of `kind` with the space `to`, else with the
/// caller's coordinator or the user, acting as the space `from`: empty, or
/// with an opening post pointing at the places in code `code`.
pub async fn open(
    daemon: &impl Daemon,
    name: String,
    kind: ThreadKind,
    to: Option<SpaceSelector>,
    from: Option<SpaceSelector>,
    code: Vec<NewCodeRef>,
) -> Result<Thread> {
    let method = ThreadMethod::OpenThread {
        name,
        kind,
        to,
        from,
        code,
    };
    Ok(*request::<Box<Thread>>(daemon, method).await?)
}

/// Add `post` with `attachments`, pointing at the places in code `code`
/// and the changes `changes`, to the thread `thread`, acting as the space
/// `from`.
pub async fn post(
    daemon: &impl Daemon,
    thread: ThreadNumber,
    post: NewPost,
    attachments: Vec<NewFile>,
    code: Vec<NewCodeRef>,
    changes: Vec<shoal_protocol::changes::ChangeNumber>,
    from: Option<SpaceSelector>,
) -> Result<Thread> {
    let method = ThreadMethod::Post {
        thread,
        post,
        attachments,
        code,
        changes,
        from,
    };
    Ok(*request::<Box<Thread>>(daemon, method).await?)
}

/// Mark the turn of `space`'s calling agent as posting its final message in
/// the thread `thread`, with the mark it replaced.
pub async fn post_in_chat(
    daemon: &impl Daemon,
    space: SpaceSelector,
    thread: ThreadNumber,
) -> Result<ChatMarked> {
    request(daemon, ThreadMethod::PostInChat { space, thread }).await
}

/// Tell the daemon the user replied `prompt` in the conversation of
/// `space`'s agent.
pub async fn replied_in_chat(
    daemon: &impl Daemon,
    space: SpaceSelector,
    prompt: Option<String>,
) -> Result<()> {
    request(daemon, ThreadMethod::RepliedInChat { space, prompt }).await
}

/// Forward the thread `thread` to `to`, else to its holder's coordinator or
/// the user, with `note`, acting as the space `from`.
pub async fn forward(
    daemon: &impl Daemon,
    thread: ThreadNumber,
    to: Option<SpaceSelector>,
    note: Option<String>,
    from: Option<SpaceSelector>,
) -> Result<Thread> {
    let method = ThreadMethod::ForwardThread {
        thread,
        to,
        note,
        from,
    };
    Ok(*request::<Box<Thread>>(daemon, method).await?)
}

/// Resolve the thread `thread` as `resolving` says, acting as the space
/// `from`.
pub async fn resolve(
    daemon: &impl Daemon,
    thread: ThreadNumber,
    resolving: Resolving,
    from: Option<SpaceSelector>,
) -> Result<Thread> {
    let method = ThreadMethod::ResolveThread {
        thread,
        resolving,
        from,
    };
    Ok(*request::<Box<Thread>>(daemon, method).await?)
}

/// The threads `space` takes part in, else the caller's, in `state`.
pub async fn list(
    daemon: &impl Daemon,
    space: Option<SpaceSelector>,
    state: ThreadState,
) -> Result<Listing> {
    let method = ThreadMethod::ListThreads {
        space,
        state,
        for_user: false,
    };
    request(daemon, method).await
}

/// The open threads the caller may read whose turn is the user's.
pub async fn for_user(daemon: &impl Daemon) -> Result<Listing> {
    let method = ThreadMethod::ListThreads {
        space: None,
        state: ThreadState::Open,
        for_user: true,
    };
    request(daemon, method).await
}

/// The thread `thread`.
pub async fn show(daemon: &impl Daemon, thread: ThreadNumber) -> Result<Thread> {
    Ok(*request::<Box<Thread>>(daemon, ThreadMethod::ShowThread { thread }).await?)
}

/// Deliver the new posts of the thread `thread` to `space`.
pub async fn link(
    daemon: &impl Daemon,
    space: SpaceSelector,
    thread: ItemNumber,
) -> Result<Thread> {
    let method = ThreadMethod::LinkThread { space, thread };
    Ok(*request::<Box<Thread>>(daemon, method).await?)
}

/// Stop delivering them.
pub async fn unlink(
    daemon: &impl Daemon,
    space: SpaceSelector,
    thread: ItemNumber,
) -> Result<Thread> {
    let method = ThreadMethod::UnlinkThread { space, thread };
    Ok(*request::<Box<Thread>>(daemon, method).await?)
}

/// The thread `item` of `space` once the other side of the post `after`
/// posts or it is resolved, waiting across daemon restarts; `restarted`
/// says the daemon restarted before the wait began.
pub async fn await_post(
    daemon: &impl Daemon,
    space: &SpaceSelector,
    item: u32,
    after: i64,
    restarted: bool,
) -> Result<Thread> {
    let method = || ThreadMethod::AwaitPost {
        space: space.clone(),
        item,
        after,
    };
    Ok(*Box::<Thread>::try_from(
        await_reply(daemon, method, restarted).await?,
    )?)
}

/// From now on the changes of `space`'s agent are for the thread `thread`,
/// or for none; the thread they are for.
pub async fn switch(
    daemon: &impl Daemon,
    space: SpaceSelector,
    thread: Option<ThreadNumber>,
) -> Result<Option<Thread>> {
    let switched: Option<Box<Thread>> =
        request(daemon, ThreadMethod::SwitchThread { space, thread }).await?;
    Ok(switched.map(|thread| *thread))
}

/// The thread the changes of `space`'s agent are for now.
pub async fn current(daemon: &impl Daemon, space: SpaceSelector) -> Result<Option<Thread>> {
    let current: Option<Box<Thread>> =
        request(daemon, ThreadMethod::CurrentThread { space }).await?;
    Ok(current.map(|thread| *thread))
}

/// Put the changes `changes`, bare numbers of `space` or else the caller's
/// own, into the thread `thread`, out of any other.
pub async fn add_changes(
    daemon: &impl Daemon,
    thread: ThreadNumber,
    space: Option<SpaceSelector>,
    changes: Vec<shoal_protocol::changes::ChangeNumber>,
) -> Result<Vec<shoal_protocol::changes::Change>> {
    let method = ThreadMethod::AddChanges {
        thread,
        space,
        changes,
    };
    request(daemon, method).await
}

/// Take the changes `changes` out of the thread `thread`.
pub async fn remove_changes(
    daemon: &impl Daemon,
    thread: ThreadNumber,
    space: Option<SpaceSelector>,
    changes: Vec<shoal_protocol::changes::ChangeNumber>,
) -> Result<Vec<shoal_protocol::changes::Change>> {
    let method = ThreadMethod::RemoveChanges {
        thread,
        space,
        changes,
    };
    request(daemon, method).await
}
