//! The daemon's answers to [`ThreadMethod`] requests, and who may send them.
use std::sync::Arc;

use anyhow::Result;

use super::super::{
    answer::Answer,
    manager::Manager,
    scope::{Caller, Scoped},
};
use super::acting::acting;
use crate::protocol::{Body, ThreadMethod};
use shoal_protocol::space_names::SpaceSelector;

pub(super) async fn handle(
    manager: &Arc<Manager>,
    method: ThreadMethod,
    caller: Option<&Caller>,
) -> Result<Answer> {
    let thread = |thread| Body::SpaceThread(Box::new(thread));
    Ok(Answer::Body(match method {
        ThreadMethod::AwaitPost { space, item, after } => {
            let (manager, caller) = (manager.clone(), caller.cloned());
            return Ok(Answer::wait(async move {
                let awaited = manager.await_post(caller.as_ref(), &space, item, after);
                let awaited = awaited.await?;
                Ok(Body::SpaceThread(Box::new(awaited)))
            }));
        }
        ThreadMethod::OpenThread {
            name,
            kind,
            to,
            from,
            code,
        } => {
            let acting = acting(manager, caller, from).await?;
            let opened = manager.open_thread(acting.as_ref(), name, kind, to.as_ref(), code);
            thread(opened.await?)
        }
        ThreadMethod::Post {
            thread: number,
            post,
            attachments,
            code,
            changes,
            from,
        } => {
            let acting = acting(manager, caller, from).await?;
            let posted =
                manager.post_to_thread(acting.as_ref(), number, post, attachments, code, changes);
            thread(posted.await?)
        }
        ThreadMethod::PostInChat {
            space,
            thread: number,
        } => Body::ChatMarked(manager.mark_chat_post(caller, &space, number).await?),
        ThreadMethod::RepliedInChat { space, prompt } => {
            let execution = caller.and_then(Caller::execution_id);
            manager.reply_in_chat(&space, execution, prompt).await?;
            Body::Ok
        }
        ThreadMethod::ForwardThread {
            thread: number,
            to,
            note,
            from,
        } => {
            let acting = acting(manager, caller, from).await?;
            let forwarded = manager.forward_thread(acting.as_ref(), number, to.as_ref(), note);
            thread(forwarded.await?)
        }
        ThreadMethod::ResolveThread {
            thread: number,
            outcome,
            choice,
            of,
            reason,
            from,
        } => {
            let acting = acting(manager, caller, from).await?;
            let resolved =
                manager.resolve_thread(acting.as_ref(), number, outcome, choice, of, reason);
            thread(resolved.await?)
        }
        ThreadMethod::ListThreads {
            space,
            state,
            for_user,
        } => Body::SpaceThreads(manager.threads(space.as_ref(), state, for_user).await?),
        ThreadMethod::ShowThread { thread: number } => {
            thread(manager.thread(caller, number).await?)
        }
        ThreadMethod::SwitchThread {
            space,
            thread: number,
        } => {
            let switched = manager.switch_thread(caller, &space, number).await?;
            Body::CurrentThread(switched.map(Box::new))
        }
        ThreadMethod::CurrentThread { space } => {
            Body::CurrentThread(manager.current_thread(&space).await?.map(Box::new))
        }
        ThreadMethod::AddChanges {
            thread: number,
            space,
            changes,
        } => Body::Changes(
            manager
                .add_changes(caller, number, space.as_ref(), &changes)
                .await?,
        ),
        ThreadMethod::RemoveChanges {
            thread: number,
            space,
            changes,
        } => {
            let removed = manager.remove_changes(caller, number, space.as_ref(), &changes);
            Body::Changes(removed.await?)
        }
        ThreadMethod::LinkThread {
            space,
            thread: number,
        } => thread(manager.link_thread(caller, &space, number, true).await?),
        ThreadMethod::UnlinkThread {
            space,
            thread: number,
        } => thread(manager.link_thread(caller, &space, number, false).await?),
    }))
}

impl Scoped for ThreadMethod {
    /// A request naming a space acts within the caller's scope: a thread
    /// opened or forwarded with a space, here or on another machine, a
    /// listing, a link, the caller's own chat. One naming a thread, or a
    /// wait in one, checks the caller takes part in it, or reads it, as it
    /// is answered.
    async fn scope(
        &mut self,
        manager: &Manager,
        caller: &Caller,
    ) -> Result<Option<&mut SpaceSelector>> {
        // A scoped caller acts as its own space.
        if let ThreadMethod::OpenThread { from, .. }
        | ThreadMethod::Post { from, .. }
        | ThreadMethod::ForwardThread { from, .. }
        | ThreadMethod::ResolveThread { from, .. } = self
        {
            *from = None;
        }
        Ok(match self {
            // The thread stays on its own machine, whichever runs the space.
            ThreadMethod::OpenThread { to: Some(to), .. }
            | ThreadMethod::ForwardThread { to: Some(to), .. } => {
                *to = SpaceSelector::id(&manager.reached_space(caller, to).await?);
                None
            }
            ThreadMethod::OpenThread { to: None, .. }
            | ThreadMethod::ForwardThread { to: None, .. } => None,
            ThreadMethod::PostInChat { space, .. }
            | ThreadMethod::RepliedInChat { space, .. }
            | ThreadMethod::LinkThread { space, .. }
            | ThreadMethod::UnlinkThread { space, .. }
            | ThreadMethod::SwitchThread { space, .. }
            | ThreadMethod::CurrentThread { space } => Some(space),
            ThreadMethod::AddChanges { space, .. } | ThreadMethod::RemoveChanges { space, .. } => {
                Some(caller.own_by_default(space))
            }
            ThreadMethod::ListThreads { space, .. } => Some(caller.own_by_default(space)),
            ThreadMethod::Post { .. }
            | ThreadMethod::AwaitPost { .. }
            | ThreadMethod::ResolveThread { .. }
            | ThreadMethod::ShowThread { .. } => None,
        })
    }
}
