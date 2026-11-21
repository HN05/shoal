//! Threads: conversations between whoever opened one, the user, a space or
//! Shoal, and whoever holds it now, the user or a space. A space opens a
//! thread with its coordinator, else the user, or with a space below it;
//! the user opens one with a space. A thread is numbered in its opener's
//! space, or in the space the user opened it with, sharing that space's
//! numbers with approval requests, and keeps its number until that space
//! goes. Posts from either side reach the other as alerts, and the spaces
//! that linked the thread; the holder forwards it, and the opener resolves
//! it with a typed outcome.
use anyhow::{Context as _, Result, bail, ensure};
use shoal_ids::space_ids::SpaceId;
use shoal_protocol::{
    code::NewCodeRef,
    space_names::SpaceSelector,
    threads::{Holder, Sender, Side, Thread, ThreadKind},
};

use crate::daemon::{alerts::Recipients, manager::Manager, notifications::Source, scope::Caller};
use posting::Opening;
use records::NewThread;

mod chat;
mod closing;
mod code;
mod delivery;
mod forwarding;
mod moving;
mod parties;
mod posting;
mod reading;
mod records;
mod resolving;

pub(crate) use chat::{number_of, record_replies as record_chat_replies};
pub(crate) use closing::{close, reopen};
pub(in crate::daemon) use code::{Followed, canonical, relative};
pub(crate) use delivery::Delivered as ThreadsDelivered;
pub(crate) use reading::{newest_waiting_for_user, waiting_count, waiting_on_spaces};

impl Manager {
    /// Open a thread of `kind` named `name` from `caller`, or from the
    /// user without one, with the space `to` names, else with the caller's
    /// coordinator or the user. An empty one notifies no one until its
    /// first post; one about the places in code `code` gets its opening
    /// post at once, named as the thread, which carries them.
    pub async fn open_thread(
        &self,
        caller: Option<&Caller>,
        name: String,
        kind: ThreadKind,
        to: Option<&SpaceSelector>,
        code: Vec<NewCodeRef>,
    ) -> Result<Thread> {
        crate::validate::message(&name).context("invalid thread name")?;
        ensure!(
            kind != ThreadKind::Review,
            "Shoal opens a review thread for each finding of a local review; start one with \
             shoal review run"
        );
        let (opener, space_id, holder) = match (caller, to) {
            // Numbered in the space it is opened with, on that space's
            // machine, which takes a request for a space this one lacks.
            (None, Some(to)) => {
                let target = self.space(to).await?.id;
                (Sender::User, target.clone(), Some(target))
            }
            (None, None) => bail!("the user opens a thread with a space; name it with --space"),
            // Numbered in the opener's space here, and held by a space here
            // or on another machine.
            (Some(caller), Some(to)) => {
                let target = self.space_id_anywhere(to).await?;
                ensure!(
                    target != caller.space_id,
                    "a space opens a thread with another space"
                );
                (Sender::Space, caller.space_id.clone(), Some(target))
            }
            (Some(caller), None) => {
                let holder = self.coordinator_of(&caller.space_id).await?;
                (Sender::Space, caller.space_id.clone(), holder)
            }
        };
        let removing = self.space_removals.clone();
        let opening = self.opening_post(caller, code).await?;
        let versioned = opening
            .as_ref()
            .map(|opening| code::versioned(&opening.code));
        let (thread, delivered) = self
            .store
            .write(move |tx| {
                // A space reserved for removal takes no thread its cleanup
                // checks have not seen.
                removing.require_taking_work(tx, &space_id)?;
                let new = NewThread {
                    kind,
                    finding: None,
                    name: &name,
                    opener,
                    space_id: &space_id,
                    holder: holder.as_ref(),
                };
                let id = records::insert_thread(tx, &new)?;
                match &opening {
                    Some(opening) => {
                        let (thread, delivered) = opening.record(tx, id, &name, &removing)?;
                        Ok((thread, Some(delivered)))
                    }
                    None => Ok((records::reread(tx, id)?, None)),
                }
            })
            .await?;
        self.signals.thread_changed();
        if let (Some(delivered), Some(versioned)) = (delivered, versioned) {
            self.announce_post(delivered, versioned).await;
        }
        Ok(thread)
    }

    /// Who makes the opening post of a thread about the places in code
    /// `code` that `caller` opens, and the references made; none without
    /// code, as an empty thread has no post. The thread's transaction
    /// records the post, so a refused one leaves no thread behind.
    async fn opening_post(
        &self,
        caller: Option<&Caller>,
        code: Vec<NewCodeRef>,
    ) -> Result<Option<Opening>> {
        if code.is_empty() {
            return Ok(None);
        }
        let code = self.new_code(caller, code).await?;
        // The opener's side is checked against no holder.
        let poster = self.poster(caller, Side::Opener, &Holder::User).await?;
        Ok(Some(Opening { poster, code }))
    }

    /// The space whose agent coordinates the space `id`, here or on another
    /// machine, or none for the user.
    async fn coordinator_of(&self, id: &SpaceId) -> Result<Option<SpaceId>> {
        Ok(match self.route_up(id, Source::Agent).await? {
            Recipients::Coordinator(id) | Recipients::Above(id) => Some(id),
            Recipients::User | Recipients::Spaces(_) => None,
        })
    }
}

/// Refuse a resolved thread, naming how to reopen it.
fn ensure_open(thread: &Thread) -> Result<()> {
    match &thread.resolution {
        None => Ok(()),
        Some(resolution) => bail!(
            "thread {} is {}; reopen it with shoal thread resolve {} --undo",
            thread.number,
            resolution.outcome.describe(),
            thread.number
        ),
    }
}

#[cfg(test)]
mod tests;
