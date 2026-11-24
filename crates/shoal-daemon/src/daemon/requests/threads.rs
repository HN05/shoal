//! The daemon's answers to [`ThreadMethod`] requests, who may send them, and
//! what each requires of the space it acts on.
use std::sync::Arc;

use anyhow::{Context as _, Result, bail};

use super::super::{
    answer::Answer,
    manager::Manager,
    scope::{Caller, Scoped},
    target_checks::{Checked, TargetCheck},
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
                let awaited = manager.coordination.await_post(
                    &*manager,
                    caller.as_ref(),
                    &space,
                    item,
                    after,
                );
                let awaited = awaited.await?;
                Ok(Body::SpaceThread(Box::new(awaited)))
            }));
        }
        ThreadMethod::OpenThread {
            opening,
            kind,
            to,
            from,
        } => {
            let acting = acting(manager, caller, from).await?;
            let opened = manager.coordination.open_thread(
                &**manager,
                acting.as_ref(),
                opening,
                kind,
                to.as_ref(),
            );
            thread(opened.await?)
        }
        ThreadMethod::OpenComment { workspace, opening } => {
            let caller = caller.context("the user's comments are opened as threads")?;
            let opened = manager
                .coordination
                .open_comment(&**manager, caller, &workspace, opening);
            opened.await?.map_or(Body::Ok, thread)
        }
        ThreadMethod::CloseOpened { opener, cause } => {
            let closed = manager.spaces.close_opened(&**manager, &opener, cause);
            closed.await?;
            Body::Ok
        }
        ThreadMethod::ReopenOpened { opener, cause } => {
            let reopened = (manager.coordination).reopen_threads(&**manager, &opener, cause);
            reopened.await?;
            Body::Ok
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
            let posted = manager.coordination.post_to_thread(
                &**manager,
                acting.as_ref(),
                number,
                post,
                attachments,
                code,
                changes,
            );
            thread(posted.await?)
        }
        ThreadMethod::PostInChat {
            space,
            thread: number,
        } => Body::ChatMarked(
            manager
                .spaces
                .mark_chat_post(&**manager, caller, &space, number)
                .await?,
        ),
        ThreadMethod::RepliedInChat { space, prompt } => {
            let execution = caller.and_then(Caller::execution_id);
            manager
                .spaces
                .reply_in_chat(&**manager, &space, execution, prompt)
                .await?;
            Body::Ok
        }
        ThreadMethod::ForwardThread {
            thread: number,
            to,
            note,
            unlink,
            from,
        } => {
            let acting = acting(manager, caller, from).await?;
            let forwarded = manager.coordination.forward_thread(
                &**manager,
                acting.as_ref(),
                number,
                to.as_ref(),
                note,
                unlink,
            );
            thread(forwarded.await?)
        }
        ThreadMethod::ResolveThread {
            thread: number,
            resolving,
            from,
        } => {
            let acting = acting(manager, caller, from).await?;
            let resolved =
                manager
                    .coordination
                    .resolve_thread(&**manager, acting.as_ref(), number, resolving);
            thread(resolved.await?)
        }
        ThreadMethod::ListThreads {
            space,
            state,
            for_user,
        } => Body::SpaceThreads(
            manager
                .coordination
                .threads(&**manager, caller, space.as_ref(), state, for_user)
                .await?,
        ),
        ThreadMethod::ShowThread { thread: number } => thread(
            manager
                .coordination
                .thread(&**manager, caller, number)
                .await?,
        ),
        ThreadMethod::SwitchThread {
            space,
            thread: number,
        } => {
            let switched = manager
                .coordination
                .switch_thread(&**manager, caller, &space, number)
                .await?;
            Body::CurrentThread(switched.map(Box::new))
        }
        ThreadMethod::CurrentThread { space } => Body::CurrentThread(
            manager
                .coordination
                .current_thread(&**manager, &space)
                .await?
                .map(Box::new),
        ),
        ThreadMethod::AddChanges {
            thread: number,
            space,
            changes,
        } => Body::Changes(
            manager
                .coordination
                .add_changes(&**manager, caller, number, space.as_ref(), &changes)
                .await?,
        ),
        ThreadMethod::RemoveChanges {
            thread: number,
            space,
            changes,
        } => {
            let removed = manager.coordination.remove_changes(
                &**manager,
                caller,
                number,
                space.as_ref(),
                &changes,
            );
            Body::Changes(removed.await?)
        }
        ThreadMethod::LinkThread {
            space,
            thread: number,
        } => thread(
            manager
                .coordination
                .link_thread(&**manager, caller, &space, number, true)
                .await?,
        ),
        ThreadMethod::UnlinkThread {
            space,
            thread: number,
        } => thread(
            manager
                .coordination
                .link_thread(&**manager, caller, &space, number, false)
                .await?,
        ),
    }))
}

impl Checked for ThreadMethod {
    fn target_check(&self) -> Option<TargetCheck> {
        match self {
            Self::PostInChat { .. }
            | Self::RepliedInChat { .. }
            | Self::ListThreads { .. }
            | Self::LinkThread { .. }
            | Self::UnlinkThread { .. }
            | Self::SwitchThread { .. }
            | Self::CurrentThread { .. }
            | Self::AddChanges { .. }
            | Self::RemoveChanges { .. }
            | Self::AwaitPost { .. }
            | Self::OpenComment { .. } => Some(TargetCheck::Space),
            Self::OpenThread { .. }
            | Self::Post { .. }
            | Self::ForwardThread { .. }
            | Self::ResolveThread { .. }
            | Self::ShowThread { .. }
            | Self::CloseOpened { .. }
            | Self::ReopenOpened { .. } => None,
        }
    }
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
                *to = SpaceSelector::id(
                    &manager
                        .coordination
                        .reached_space(manager, caller, to)
                        .await?,
                );
                None
            }
            ThreadMethod::OpenThread { to: None, .. }
            | ThreadMethod::ForwardThread { to: None, .. } => None,
            ThreadMethod::OpenComment { workspace, .. } => Some(workspace),
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
            // Only another machine's daemon sends them, as the user.
            ThreadMethod::CloseOpened { .. } | ThreadMethod::ReopenOpened { .. } => {
                bail!(
                    "only another machine's daemon closes and reopens the threads its space opened"
                )
            }
        })
    }
}
