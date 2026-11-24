//! Posts an agent makes in its conversation: the turn it marks with the
//! thread (see [`crate::chat_marks`]) posts its final message
//! there. A prompt the user submits in that conversation answers the post:
//! Shoal records it as the user's reply.
use crate::daemon::Records;
use crate::{Coordination, daemon::Daemon};
use anyhow::{Result, ensure};
use rusqlite::{Connection, Transaction};
use shoal_ids::space_ids::SpaceId;
use shoal_protocol::{
    item_numbers::ItemNumber,
    threads::{Holder, NewPost, Post, Sender, Side, Thread, ThreadNumber, ThreadSpace},
};

use super::{
    delivery::{self, Delivered, Party, Texts},
    ensure_open,
    posting::{Attached, Poster, record_post},
    records,
};
use crate::{model::Space, scope::Caller};

/// The number of the thread of record ID `id`.
pub fn number_of(db: &Connection, id: i64) -> Result<ItemNumber> {
    Ok(records::reread(db, id)?.number)
}

/// The reply in the chat that a prompt without text, or one too long for
/// its thread, records.
const REPLIED: &str = "The user replied in the chat.";

/// The post an agent makes in its chat: its turn's final `message`, kept
/// to the length and characters of posts. Without one, as from an agent
/// whose hook does not pass it, the post points to the chat.
fn in_chat(message: Option<&str>) -> NewPost {
    NewPost {
        text: chat_text(message, crate::alerts::WROTE_IN_CHAT),
        ..NewPost::default()
    }
}

/// `message`, a turn's message or a prompt from the chat, kept to the
/// length and characters of posts, or `fallback` without one.
fn chat_text(message: Option<&str>, fallback: &str) -> String {
    let text: String = (message.unwrap_or_default().chars())
        .map(|c| if c.is_control() && c != '\n' { ' ' } else { c })
        .collect();
    match text.trim() {
        "" => fallback.to_owned(),
        text => crate::validate::shorten(text.to_owned(), shoal_protocol::threads::MAX_POST_LEN),
    }
}

/// The side of `thread` the space `id` posts from: the opener's when it
/// opened the thread, the holder's while it holds it, else none.
fn side_of(thread: &Thread, id: &SpaceId) -> Option<Side> {
    match (thread.opener, &thread.holder) {
        _ if thread.opener_id() == Some(id) => Some(Side::Opener),
        (_, Holder::Space(holder)) if &holder.id == id => Some(Side::Holder),
        _ => None,
    }
}

/// The open threads whose last post from a side the space `space_id` made
/// in its chat, each with that post, which a reply there answers.
fn answered_in_chat(tx: &Transaction<'_>, space_id: &SpaceId) -> Result<Vec<(Thread, Post)>> {
    let open = records::query(
        tx,
        &format!(
            "WHERE {} AND EXISTS (SELECT 1 FROM thread_posts
                WHERE thread_id=t.id AND sender_id=?1 AND in_chat)",
            records::OPEN
        ),
        [space_id],
    )?;
    Ok(open
        .into_iter()
        .filter_map(|thread| {
            let last = thread.last_turn()?.clone();
            let ours = last.in_chat
                && last
                    .space
                    .as_ref()
                    .is_some_and(|space| &space.id == space_id);
            ours.then_some((thread, last))
        })
        .collect())
}

/// Record the user's reply in the chat of the space `space_id`, `prompt`
/// when the hook passed it, to each post it made there that its thread
/// waits on. The party the post went to loses its unread alert of it, or
/// hears of the reply once it read it.
pub fn record_replies(
    tx: &Transaction<'_>,
    outside: &dyn Records,
    space_id: &SpaceId,
    prompt: Option<&str>,
) -> Result<Delivered> {
    let text = chat_text(prompt, REPLIED);
    let mut delivered = Delivered::default();
    for (thread, post) in answered_in_chat(tx, space_id)? {
        // A post in a chat comes from a side, which only its parties take.
        let Some(side) = post.side.map(Side::other) else {
            continue;
        };
        let reply = NewPost {
            text: text.clone(),
            ..NewPost::default()
        };
        let reply_id = records::insert_post(
            tx,
            &records::NewRecord {
                thread: thread.id,
                author: records::Author {
                    side: Some(side),
                    sender: Sender::User,
                    space: None,
                },
                post: &reply,
                files: &[],
                code: &[],
                in_chat: true,
                forwards: None,
            },
        )?;
        // A prompt too long for the thread records only that the user
        // replied, which the agent read in its chat.
        if records::ensure_fits(&records::reread(tx, thread.id)?).is_err() {
            records::replace_text(tx, reply_id, REPLIED)?;
        }
        let Some(party) = Party::of(&thread, side) else {
            continue;
        };
        let retracted = match post.alert_id {
            Some(alert) => delivery::retract_alert(tx, alert)?,
            None => false,
        };
        if !retracted && party != Party::User {
            let thread = records::reread(tx, thread.id)?;
            let text = format!(
                "The user replied in the chat to post {} of thread {} ({}); it needs no reply from you.",
                post.id,
                thread.number,
                thread.one_line()
            );
            delivery::deliver(
                tx,
                outside,
                &thread,
                None,
                &party,
                Texts::notice(text),
                &mut delivered,
            )?;
        }
    }
    Ok(delivered)
}

impl Coordination {
    /// The thread `number` that the agent of `space`, as `caller`, marks
    /// its turn to post in: an open one it opened or holds.
    pub async fn chat_post_thread(
        &self,
        daemon: &dyn Daemon,
        caller: Option<&Caller>,
        space: &Space,
        number: ThreadNumber,
    ) -> Result<Thread> {
        let thread = self.readable_thread(daemon, caller, number).await?;
        // The turn's post goes in from the side the space itself takes,
        // which a space above the holder, posting with `send`, does not.
        ensure!(
            side_of(&thread, &space.id).is_some(),
            "{} posts in its chat only in a thread it opened or holds; post in thread {} with shoal send",
            space.name,
            thread.number
        );
        ensure_open(&thread)?;
        Ok(thread)
    }

    /// Post `message`, the final message of a turn the agent of `space`
    /// marked, in the thread of record ID `id`.
    pub async fn post_marked(
        &self,
        daemon: &dyn Daemon,
        space: &Space,
        id: i64,
        message: Option<String>,
    ) -> Result<()> {
        let post = in_chat(message.as_deref());
        let outside = self.records.clone();
        let space = space.clone();
        let (delivered, answered) = self
            .store
            .write(move |tx| {
                let thread = records::reread(tx, id)?;
                // The thread may have gone to another holder since the turn
                // was marked, which leaves the space no side to post from.
                let Some(side) = side_of(&thread, &space.id) else {
                    crate::log!(
                        "post of {} in its chat dropped: it no longer takes part in thread {}",
                        space.name,
                        thread.number
                    );
                    return Ok((Delivered::default(), false));
                };
                let poster = Poster {
                    side: Some(side),
                    sender: Sender::Space,
                    space: Some(ThreadSpace::of(&space)),
                    seen: thread.holder.clone(),
                };
                let (thread, delivered) =
                    record_post(tx, id, &poster, &post, Attached::default(), true, &*outside)?;
                Ok((delivered, super::findings::awaits_reviewer(&thread)))
            })
            .await?;
        self.announce_threads(daemon, delivered).await;
        if answered {
            daemon.answer_posts();
        }
        Ok(())
    }
}
