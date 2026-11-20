//! Threads: conversations between whoever opened one and whoever holds it.
use anyhow::Result;
use shoal_protocol::{
    files::NewFile,
    item_numbers::ItemNumber,
    protocol::ThreadMethod,
    space_names::SpaceSelector,
    threads::{Listing, NewPost, OutcomeKind, Thread, ThreadKind, ThreadNumber, ThreadState},
};

use crate::connection::{Daemon, await_reply, request};

/// Open an empty thread `name` of `kind` with the space `to`, else with
/// the caller's coordinator or the user, acting as the space `from`.
pub async fn open(
    daemon: &impl Daemon,
    name: String,
    kind: ThreadKind,
    to: Option<SpaceSelector>,
    from: Option<SpaceSelector>,
) -> Result<Thread> {
    let method = ThreadMethod::OpenThread {
        name,
        kind,
        to,
        from,
    };
    Ok(*request::<Box<Thread>>(daemon, method).await?)
}

/// Add `post` with `attachments` to the thread `thread`, acting as the
/// space `from`.
pub async fn post(
    daemon: &impl Daemon,
    thread: ThreadNumber,
    post: NewPost,
    attachments: Vec<NewFile>,
    from: Option<SpaceSelector>,
) -> Result<Thread> {
    let method = ThreadMethod::Post {
        thread,
        post,
        attachments,
        from,
    };
    Ok(*request::<Box<Thread>>(daemon, method).await?)
}

/// Mark the turn of `space`'s calling agent as posting its final message in
/// the thread `thread`.
pub async fn post_in_chat(
    daemon: &impl Daemon,
    space: SpaceSelector,
    thread: ThreadNumber,
) -> Result<Thread> {
    let method = ThreadMethod::PostInChat { space, thread };
    Ok(*request::<Box<Thread>>(daemon, method).await?)
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

/// What resolving a thread records: an outcome with its data, or none to
/// reopen it.
#[derive(Debug, Default)]
pub struct Resolving {
    pub outcome: Option<OutcomeKind>,
    pub choice: Option<u32>,
    pub of: Option<ThreadNumber>,
    pub reason: Option<String>,
}

/// Resolve the thread `thread` as `resolving` says, acting as the space
/// `from`.
pub async fn resolve(
    daemon: &impl Daemon,
    thread: ThreadNumber,
    resolving: Resolving,
    from: Option<SpaceSelector>,
) -> Result<Thread> {
    let Resolving {
        outcome,
        choice,
        of,
        reason,
    } = resolving;
    let method = ThreadMethod::ResolveThread {
        thread,
        outcome,
        choice,
        of,
        reason,
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
    request(daemon, ThreadMethod::ListThreads { space, state }).await
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
