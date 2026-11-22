//! The mark an agent puts on its turn with `shoal send --in-chat`, on the
//! execution it runs as, a chat's as a workspace's: once the turn ends,
//! its final message goes as a post in a thread or as an alert (see
//! [`alerts`]). A turn has one mark,
//! which a later one replaces. A prompt the user submits in the agent's
//! conversation answers the turn instead, which drops its mark.
use anyhow::{Result, bail, ensure};
use rusqlite::{Connection, OptionalExtension as _, Transaction, params};
use shoal_ids::execution_ids::ExecutionId;
use shoal_protocol::{
    chat_marks::{ChatMark, ChatMarked},
    space_names::SpaceSelector,
    threads::ThreadNumber,
};

use crate::{
    daemon::{manager::Manager, scope::Caller},
    model::Space,
};

/// What a message from the chat says without the agent's own text, as
/// from an agent whose hook does not pass its turn's final message.
pub(crate) const WROTE_IN_CHAT: &str =
    "The agent wrote in its chat; attach to its session to read it.";

mod alerts;
mod unsent;

use alerts::MarkedAlert;

/// What a marked turn's final message becomes, as its record keeps it.
enum Mark {
    /// A post in the thread of this record ID.
    Post(i64),
    /// An alert.
    Alert(MarkedAlert),
}

/// Mark the turn of the agent `execution` runs with `mark`, returning what
/// the mark it replaced showed.
fn put(tx: &Transaction<'_>, execution: &ExecutionId, mark: &Mark) -> Result<Option<ChatMark>> {
    let replaced = match take(tx, execution)? {
        Some(replaced) => Some(shown(tx, &replaced)?),
        None => None,
    };
    let (thread, alert) = match mark {
        Mark::Post(thread) => (Some(*thread), None),
        Mark::Alert(alert) => (None, Some(serde_json::to_string(alert)?)),
    };
    tx.execute(
        "INSERT INTO chat_marks(execution_id,thread_id,alert) VALUES (?1,?2,?3)",
        params![execution, thread, alert],
    )?;
    Ok(replaced)
}

/// Clear the mark of the turn of the agent `execution` runs, returning it.
fn take(tx: &Transaction<'_>, execution: &ExecutionId) -> Result<Option<Mark>> {
    let row: Option<(Option<i64>, Option<String>)> = tx
        .query_row(
            "SELECT thread_id,alert FROM chat_marks WHERE execution_id=?1",
            [execution],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()?;
    tx.execute("DELETE FROM chat_marks WHERE execution_id=?1", [execution])?;
    Ok(match row {
        None => None,
        Some((Some(thread), _)) => Some(Mark::Post(thread)),
        Some((None, Some(alert))) => Some(Mark::Alert(serde_json::from_str(&alert)?)),
        Some((None, None)) => bail!("a turn's mark names neither a thread nor an alert"),
    })
}

/// Clear the mark of the turn of the agent `execution` runs, returning it
/// with what it shows the agent.
fn taken(tx: &Transaction<'_>, execution: &ExecutionId) -> Result<Option<(Mark, ChatMark)>> {
    let Some(mark) = take(tx, execution)? else {
        return Ok(None);
    };
    let shown = shown(tx, &mark)?;
    Ok(Some((mark, shown)))
}

/// What `mark` shows the agent: the thread its post goes to, or whom its
/// alert reaches.
fn shown(db: &Connection, mark: &Mark) -> Result<ChatMark> {
    Ok(match mark {
        Mark::Post(id) => ChatMark::Post {
            thread: crate::daemon::threads::number_of(db, *id)?,
        },
        Mark::Alert(alert) => ChatMark::Alert {
            to: alert.recipients.clone(),
        },
    })
}

impl Manager {
    /// The execution whose turn the agent of `space` that `caller` runs as
    /// marks, refusing an execution that runs no agent of the space, or one
    /// without Shoal's hook that ends its turns.
    async fn marked_turn(&self, caller: Option<&Caller>, space: &Space) -> Result<ExecutionId> {
        let execution = caller.and_then(Caller::execution_id);
        let (execution, agent) = self.agents.agent_turn(space, execution).await?;
        let settings = self.repositories.space_settings(self, space).await?;
        ensure!(
            settings.integration(&agent).reports_turn_ends(),
            "agent {agent} has no Shoal hook that ends its turns; give the text to shoal send instead"
        );
        Ok(execution)
    }

    /// Mark the turn `turn` with `mark`.
    async fn mark_turn(&self, turn: ExecutionId, mark: Mark) -> Result<ChatMarked> {
        self.store
            .write(move |tx| {
                let replaced = put(tx, &turn, &mark)?;
                let mark = shown(tx, &mark)?;
                Ok(ChatMarked { mark, replaced })
            })
            .await
    }

    /// Mark the current turn of `caller`'s agent in the space `selector`
    /// names as posting in the thread `number` in its conversation.
    pub(crate) async fn mark_chat_post(
        &self,
        caller: Option<&Caller>,
        selector: &SpaceSelector,
        number: ThreadNumber,
    ) -> Result<ChatMarked> {
        let space = self.space(selector).await?;
        let turn = self.marked_turn(caller, &space).await?;
        let thread = self.chat_post_thread(caller, &space, number).await?;
        self.mark_turn(turn, Mark::Post(thread.id)).await
    }

    /// Send the turn's `final_message` as the agent `execution` runs in the
    /// space `selector` names marked its turn to, consuming the mark. The
    /// agent's turn ends either way, so a failure is logged and told to the
    /// agent, which may send the message again.
    pub(crate) async fn end_marked_turn(
        &self,
        selector: &SpaceSelector,
        execution: Option<&ExecutionId>,
        final_message: Option<String>,
    ) {
        let Some(execution) = execution else {
            return;
        };
        let sent = self.send_marked(selector, execution.to_owned(), final_message);
        if let Err(error) = sent.await {
            crate::daemon::log!("final message of {selector}'s turn not sent: {error:#}");
        }
    }

    async fn send_marked(
        &self,
        selector: &SpaceSelector,
        turn: ExecutionId,
        message: Option<String>,
    ) -> Result<()> {
        let space = self.space(selector).await?;
        let taken = {
            let turn = turn.clone();
            self.store.write(move |tx| taken(tx, &turn)).await?
        };
        let Some((mark, shown)) = taken else {
            return Ok(());
        };
        let sent = match mark {
            Mark::Post(thread) => self.post_marked(&space, thread, message).await,
            Mark::Alert(alert) => (self.send_marked_alert(&space, turn, alert, message)).await,
        };
        if let Err(error) = &sent {
            let why = format!("sending it failed: {error:#}");
            self.tell_unsent(&space, &shown, &why).await;
        }
        sent
    }

    /// The user replied `prompt` in the conversation of the agent
    /// `execution` runs in the space `selector` names: record their
    /// interaction with it, drop the mark of a turn underway, telling the
    /// agent of an alert it leaves unsent, record their reply to the posts
    /// its agents made there, and mark the alerts they sent the user from
    /// there read.
    pub(crate) async fn reply_in_chat(
        &self,
        selector: &SpaceSelector,
        execution: Option<&ExecutionId>,
        prompt: Option<String>,
    ) -> Result<()> {
        self.agents.note_interaction(self, selector).await;
        let space = self.space(selector).await?;
        let (turn, id) = (execution.cloned(), space.id.clone());
        let (dropped, delivered) = self
            .store
            .write(move |tx| {
                let dropped = match &turn {
                    Some(turn) => taken(tx, turn)?,
                    None => None,
                };
                alerts::read_in_chat(tx, &id)?;
                let delivered =
                    crate::daemon::threads::record_chat_replies(tx, &id, prompt.as_deref())?;
                Ok((dropped, delivered))
            })
            .await?;
        self.announce_threads(delivered).await;
        // The prompt answers a post the turn marked; an alert goes unsent.
        if let Some((Mark::Alert(_), shown)) = dropped {
            let why = "the user's prompt answered your turn";
            self.tell_unsent(&space, &shown, why).await;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests;
