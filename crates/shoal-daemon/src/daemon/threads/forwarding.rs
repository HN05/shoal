//! Forwarding a thread: its holder gives it to a space its scope reaches,
//! or up to its coordinator or the user, with a note, and the new holder
//! hears what it says so far.
use anyhow::{Context as _, Result, bail, ensure};
use shoal_ids::space_ids::SpaceId;
use shoal_protocol::{
    space_names::SpaceSelector,
    threads::{Holder, NewPost, Post, Sender, Side, Thread, ThreadNumber},
};

use super::{
    delivery::{self, Delivered, Party, Texts},
    ensure_open,
    records::{self, NewRecord},
};
use crate::daemon::{manager::Manager, scope::Caller};

impl Manager {
    /// Give the thread `number`, which `caller` holds, to the space `to`
    /// names, else to its holder's coordinator or the user, with `note` as
    /// a post; the new holder receives the thread's posts from then on.
    pub async fn forward_thread(
        &self,
        caller: Option<&Caller>,
        number: ThreadNumber,
        to: Option<&SpaceSelector>,
        note: Option<String>,
    ) -> Result<Thread> {
        let thread = self.readable_thread(caller, number).await?;
        self.ensure_forwards(caller, &thread).await?;
        ensure_open(&thread)?;
        let note = note.unwrap_or_default();
        if !note.is_empty() {
            crate::validate::agent_message(&note).context("invalid note")?;
        }
        let holder = self.next_holder(&thread, to).await?;
        let poster = self.poster(caller, Side::Holder, &thread.holder).await?;
        let id = thread.id;
        let (thread, delivered) = self
            .store
            .write(move |tx| {
                let current = records::reread(tx, id)?;
                ensure_open(&current)?;
                records::ensure_held_by(&current, &poster.seen)?;
                let post = NewPost {
                    text: note,
                    ..NewPost::default()
                };
                records::insert_post(
                    tx,
                    &NewRecord {
                        thread: id,
                        author: poster.author(),
                        post: &post,
                        files: &[],
                        in_chat: false,
                        forwards: Some(holder.as_ref()),
                    },
                )?;
                records::set_holder(tx, id, holder.as_ref())?;
                let thread = records::reread(tx, id)?;
                records::ensure_fits(&thread)?;
                let mut delivered = Delivered::default();
                let forward = thread
                    .posts
                    .last()
                    .context("the forward was not recorded")?;
                if thread.posts.iter().any(Post::speaks) {
                    let to = Party::of(&thread, Side::Holder).context("a thread has a holder")?;
                    let texts = Texts::forwarded(&thread, forward);
                    delivery::deliver(tx, &thread, Some(forward), &to, texts, &mut delivered)?;
                }
                Ok((thread, delivered))
            })
            .await?;
        self.announce_threads(delivered).await;
        Ok(thread)
    }

    /// Whom `thread` goes to next: the space `to` names, else its holder's
    /// coordinator or the user; never its holder, nor the space that
    /// opened it.
    async fn next_holder(
        &self,
        thread: &Thread,
        to: Option<&SpaceSelector>,
    ) -> Result<Option<SpaceId>> {
        let holder = match (to, &thread.holder) {
            (Some(to), _) => Some(self.space(to).await?.id),
            (None, Holder::Space(holder)) => self.coordinator_of(&holder.id).await?,
            (None, Holder::User) => {
                bail!(
                    "the user holds thread {}; name a space to forward it to",
                    thread.number
                )
            }
        };
        ensure!(
            holder.as_ref() != thread.holder.space().map(|space| &space.id),
            "{} holds thread {} already",
            thread.holder.label(),
            thread.number
        );
        let opener = match thread.opener {
            Sender::Space => holder.as_ref() == Some(&thread.space_id),
            Sender::User => holder.is_none(),
            Sender::Shoal => false,
        };
        ensure!(
            !opener,
            "{} opened thread {}; it cannot hold it too",
            thread.party(Side::Opener),
            thread.number
        );
        Ok(holder)
    }
}
