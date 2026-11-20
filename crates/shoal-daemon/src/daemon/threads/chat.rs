//! Posts an agent makes in its conversation: it marks its turn with the
//! thread, and the turn's final message becomes its post there. A prompt
//! the user submits in that conversation answers the post: Shoal records
//! it as the user's reply. An agent marks the turn of the execution it
//! runs as, a chat's as a workspace's.
use anyhow::{Result, ensure};
use rusqlite::{Connection, OptionalExtension as _, Transaction, params};
use shoal_ids::{execution_ids::ExecutionId, space_ids::SpaceId};
use shoal_protocol::{
    space_names::SpaceSelector,
    threads::{Holder, NewPost, Post, Sender, Side, Thread, ThreadNumber},
};

use super::{
    delivery::{self, Delivered, Party, Texts},
    ensure_open,
    posting::{Poster, record_post},
    records,
};
use crate::daemon::{manager::Manager, scope::Caller};

/// Mark the current turn of the agent `execution` runs as posting in the
/// thread `thread` in its conversation, or clear the mark without one;
/// whether that changed the mark.
fn set_mark(db: &Connection, execution: &ExecutionId, thread: Option<i64>) -> Result<bool> {
    let changed = db.execute(
        "UPDATE executions SET chat_thread=?2 WHERE id=?1 AND chat_thread IS NOT ?2",
        params![execution, thread],
    )?;
    Ok(changed > 0)
}

/// Clear the mark of the turn of the agent `execution` runs, returning the
/// thread it named.
fn take_mark(tx: &Transaction<'_>, execution: &ExecutionId) -> Result<Option<i64>> {
    let thread: Option<i64> = tx
        .query_row(
            "SELECT chat_thread FROM executions WHERE id=?1",
            [execution],
            |row| row.get(0),
        )
        .optional()?
        .flatten();
    set_mark(tx, execution, None)?;
    Ok(thread)
}

/// The reply in the chat that a prompt without text, or one too long for
/// its thread, records.
const REPLIED: &str = "The user replied in the chat.";

/// The post an agent makes in its chat: its turn's final `message`, kept
/// to the length and characters of posts. Without one, as from an agent
/// whose hook does not pass it, the post points to the chat.
fn in_chat(message: Option<&str>) -> NewPost {
    let fallback = "The agent posted in its chat; attach to its session to read it.";
    NewPost {
        text: chat_text(message, fallback),
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
        (Sender::Space, _) if &thread.space_id == id => Some(Side::Opener),
        (_, Holder::Space(holder)) if &holder.id == id => Some(Side::Holder),
        _ => None,
    }
}

/// The open threads whose last post the space `space_id` made in its
/// chat, each with that post, which a reply there answers.
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
            let last = thread.last_post()?.clone();
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
fn record_replies(
    tx: &Transaction<'_>,
    space_id: &SpaceId,
    prompt: Option<&str>,
) -> Result<Delivered> {
    let text = chat_text(prompt, REPLIED);
    let mut delivered = Delivered::default();
    for (thread, post) in answered_in_chat(tx, space_id)? {
        let side = match post.side {
            Side::Opener => Side::Holder,
            Side::Holder => Side::Opener,
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
                    side,
                    sender: Sender::User,
                    space: None,
                },
                post: &reply,
                files: &[],
                in_chat: true,
                forwards: None,
            },
        )?;
        // A prompt too long for the thread records only that the user
        // replied, which the agent read in its chat.
        if records::ensure_fits(&records::reread(tx, thread.id)?).is_err() {
            tx.execute(
                "UPDATE thread_posts SET text=?2 WHERE id=?1",
                rusqlite::params![reply_id, REPLIED],
            )?;
        }
        let Some(party) = Party::of(&thread, side) else {
            continue;
        };
        let retracted = match post.alert_id {
            Some(alert) => tx.execute("DELETE FROM alerts WHERE id=?1 AND read=0", [alert])? > 0,
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

impl Manager {
    /// Mark the current turn of `caller`'s agent in the space `selector`
    /// names as posting in the thread `number` in its conversation, which
    /// needs Shoal's hook that ends the agent's turns.
    pub(crate) async fn mark_chat_post(
        &self,
        caller: Option<&Caller>,
        selector: &SpaceSelector,
        number: ThreadNumber,
    ) -> Result<Thread> {
        let space = self.space(selector).await?;
        let execution = caller.and_then(Caller::execution_id);
        let (turn, agent) = self.agent_turn(&space, execution).await?;
        let settings = self.space_settings(&space).await?;
        ensure!(
            settings.integration(&agent).reports_turn_ends(),
            "agent {agent} has no Shoal hook that ends its turns; post with shoal send <thread> \"<text>\""
        );
        let thread = self.readable_thread(caller, number).await?;
        self.side(caller, &thread).await?;
        ensure_open(&thread)?;
        let id = thread.id;
        self.store
            .run(move |db| set_mark(db, &turn, Some(id)))
            .await?;
        Ok(thread)
    }

    /// Post the turn's `final_message` in the thread the agent `execution`
    /// runs in the space `selector` names marked its turn with, consuming
    /// the mark. The agent's turn ends either way, so a failure is only
    /// logged.
    pub(crate) async fn post_marked_chat(
        &self,
        selector: &SpaceSelector,
        execution: Option<&ExecutionId>,
        final_message: Option<String>,
    ) {
        let Some(execution) = execution else {
            return;
        };
        let posted = self.post_in_chat(selector, execution.to_owned(), final_message);
        if let Err(error) = posted.await {
            crate::daemon::log!("post of {selector} in its chat not recorded: {error:#}");
        }
    }

    async fn post_in_chat(
        &self,
        selector: &SpaceSelector,
        turn: ExecutionId,
        message: Option<String>,
    ) -> Result<()> {
        let space = self.space(selector).await?;
        let post = in_chat(message.as_deref());
        let removing = self.space_removals.clone();
        let delivered = self
            .store
            .write(move |tx| {
                let Some(id) = take_mark(tx, &turn)? else {
                    return Ok(Delivered::default());
                };
                let thread = records::reread(tx, id)?;
                // The thread may have gone to another holder since the turn
                // was marked, which leaves the space no side to post from.
                let Some(side) = side_of(&thread, &space.id) else {
                    crate::daemon::log!(
                        "post of {} in its chat dropped: it no longer takes part in thread {}",
                        space.name,
                        thread.number
                    );
                    return Ok(Delivered::default());
                };
                let poster = Poster {
                    side,
                    space: Some((space.id.clone(), space.name.clone())),
                    seen: thread.holder.clone(),
                };
                let (_, delivered) = record_post(tx, id, &poster, &post, &[], true, &removing)?;
                Ok(delivered)
            })
            .await?;
        self.announce_threads(delivered).await;
        Ok(())
    }

    /// The user replied `prompt` in the conversation of the agent
    /// `execution` runs in the space `selector` names: record their
    /// interaction with it, clear the mark of a turn underway, and record
    /// their reply to the posts its agents made there.
    pub(crate) async fn reply_in_chat(
        &self,
        selector: &SpaceSelector,
        execution: Option<&ExecutionId>,
        prompt: Option<String>,
    ) -> Result<()> {
        self.note_interaction(selector).await;
        let space = self.space(selector).await?;
        let turn = execution.cloned();
        let delivered = self
            .store
            .write(move |tx| {
                if let Some(turn) = &turn {
                    set_mark(tx, turn, None)?;
                }
                record_replies(tx, &space.id, prompt.as_deref())
            })
            .await?;
        self.announce_threads(delivered).await;
        Ok(())
    }
}
