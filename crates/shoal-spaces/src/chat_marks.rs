//! The mark `shoal send --in-chat` puts on an agent's turn: once the turn
//! ends, its final message goes as a post in a thread or as an alert. A
//! turn has one mark, which a later one replaces, and a prompt the user
//! submits answers the turn instead and drops the mark.
use crate::Spaces;
use crate::daemon::Daemon;
use anyhow::{Result, bail, ensure};
use rusqlite::{Connection, OptionalExtension as _, Transaction, params};
use shoal_ids::execution_ids::ExecutionId;
use shoal_protocol::{
    chat_marks::{ChatMark, ChatMarked},
    space_names::SpaceSelector,
    threads::ThreadNumber,
};

use crate::{model::Space, scope::Caller};

/// Stands in for the agent's text when its hook does not pass the turn's
/// final message.
pub const WROTE_IN_CHAT: &str = "The agent wrote in its chat; attach to its session to read it.";

mod alerts;
mod unsent;

use alerts::MarkedAlert;

enum Mark {
    /// A post in the thread of this record ID.
    Post(i64),
    Alert(MarkedAlert),
}

/// Returns what the replaced mark showed.
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

/// Also returns what the mark shows the agent.
fn taken(tx: &Transaction<'_>, execution: &ExecutionId) -> Result<Option<(Mark, ChatMark)>> {
    let Some(mark) = take(tx, execution)? else {
        return Ok(None);
    };
    let shown = shown(tx, &mark)?;
    Ok(Some((mark, shown)))
}

/// The thread the post goes to, or whom the alert reaches.
fn shown(db: &Connection, mark: &Mark) -> Result<ChatMark> {
    Ok(match mark {
        Mark::Post(id) => ChatMark::Post {
            thread: crate::threads::number_of(db, *id)?,
        },
        Mark::Alert(alert) => ChatMark::Alert {
            to: alert.recipients.clone(),
        },
    })
}

impl Spaces {
    /// Refuses an execution that runs no agent of the space, or one without
    /// Shoal's hook that ends its turns.
    async fn marked_turn(
        &self,
        daemon: &dyn Daemon,
        caller: Option<&Caller>,
        space: &Space,
    ) -> Result<ExecutionId> {
        let execution = caller.and_then(Caller::execution_id);
        let (execution, agent) = daemon.agent_turn(space, execution).await?;
        let settings = daemon.space_settings(space).await?;
        ensure!(
            settings.integration(&agent).reports_turn_ends(),
            "agent {agent} has no Shoal hook that ends its turns; give the text to shoal send instead"
        );
        Ok(execution)
    }

    async fn mark_turn(&self, turn: ExecutionId, mark: Mark) -> Result<ChatMarked> {
        self.store
            .write(move |tx| {
                let replaced = put(tx, &turn, &mark)?;
                let mark = shown(tx, &mark)?;
                Ok(ChatMarked { mark, replaced })
            })
            .await
    }

    pub async fn mark_chat_post(
        &self,
        daemon: &dyn Daemon,
        caller: Option<&Caller>,
        selector: &SpaceSelector,
        number: ThreadNumber,
    ) -> Result<ChatMarked> {
        let space = self.space(selector).await?;
        let turn = self.marked_turn(daemon, caller, &space).await?;
        let thread = daemon
            .coordination()
            .chat_post_thread(daemon, caller, &space, number)
            .await?;
        self.mark_turn(turn, Mark::Post(thread.id)).await
    }

    /// Consumes the mark. The turn ends either way, so a failure is logged
    /// and told to the agent, which may send the message again.
    pub async fn end_marked_turn(
        &self,
        daemon: &dyn Daemon,
        selector: &SpaceSelector,
        execution: Option<&ExecutionId>,
        final_message: Option<String>,
    ) {
        let Some(execution) = execution else {
            return;
        };
        let sent = self.send_marked(daemon, selector, execution.to_owned(), final_message);
        if let Err(error) = sent.await {
            crate::log!("final message of {selector}'s turn not sent: {error:#}");
        }
    }

    async fn send_marked(
        &self,
        daemon: &dyn Daemon,
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
            Mark::Post(thread) => {
                daemon
                    .coordination()
                    .post_marked(daemon, &space, thread, message)
                    .await
            }
            Mark::Alert(alert) => {
                (self.send_marked_alert(daemon, &space, turn, alert, message)).await
            }
        };
        if let Err(error) = &sent {
            let why = format!("sending it failed: {error:#}");
            self.tell_unsent(daemon, &space, &shown, &why).await;
        }
        sent
    }

    /// Records the user's interaction with the agent, drops the mark of a
    /// turn under way, telling the agent of an alert it leaves unsent,
    /// records the reply to the posts its agents made there, and marks read
    /// the alerts they sent the user from there.
    pub async fn reply_in_chat(
        &self,
        daemon: &dyn Daemon,
        selector: &SpaceSelector,
        execution: Option<&ExecutionId>,
        prompt: Option<String>,
    ) -> Result<()> {
        daemon.note_interaction(selector).await;
        let space = self.space(selector).await?;
        let (turn, id) = (execution.cloned(), space.id.clone());
        let records = self.records.clone();
        let (dropped, delivered) = self
            .store
            .write(move |tx| {
                let dropped = match &turn {
                    Some(turn) => taken(tx, turn)?,
                    None => None,
                };
                crate::alerts::read_in_chat(tx, &id)?;
                let delivered = crate::threads::record_chat_replies(
                    tx,
                    records.coordination(),
                    &id,
                    prompt.as_deref(),
                )?;
                Ok((dropped, delivered))
            })
            .await?;
        daemon
            .coordination()
            .announce_threads(daemon, delivered)
            .await;
        // The prompt answers a post the turn marked; an alert goes unsent.
        if let Some((Mark::Alert(_), shown)) = dropped {
            let why = "the user's prompt answered your turn";
            self.tell_unsent(daemon, &space, &shown, why).await;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests;
