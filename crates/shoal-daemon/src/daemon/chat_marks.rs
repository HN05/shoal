//! The mark an agent puts on its turn with `shoal send --in-chat`, on the
//! execution it runs as, a chat's as a workspace's: once the turn ends,
//! its final message goes as a post in a thread. A turn has one mark,
//! which a later one replaces. A prompt the user submits in the agent's
//! conversation answers the turn instead, which drops its mark.
use anyhow::{Result, ensure};
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

/// What a marked turn's final message becomes, as its record keeps it.
enum Mark {
    /// A post in the thread of this record ID.
    Post(i64),
}

/// Mark the turn of the agent `execution` runs with `mark`, returning what
/// the mark it replaced showed.
fn put(tx: &Transaction<'_>, execution: &ExecutionId, mark: &Mark) -> Result<Option<ChatMark>> {
    let replaced = match take(tx, execution)? {
        Some(replaced) => Some(shown(tx, &replaced)?),
        None => None,
    };
    let Mark::Post(thread) = mark;
    tx.execute(
        "INSERT INTO chat_marks(execution_id,thread_id) VALUES (?1,?2)",
        params![execution, thread],
    )?;
    Ok(replaced)
}

/// Clear the mark of the turn of the agent `execution` runs, returning it.
fn take(tx: &Transaction<'_>, execution: &ExecutionId) -> Result<Option<Mark>> {
    let thread: Option<Option<i64>> = tx
        .query_row(
            "SELECT thread_id FROM chat_marks WHERE execution_id=?1",
            [execution],
            |row| row.get(0),
        )
        .optional()?;
    tx.execute("DELETE FROM chat_marks WHERE execution_id=?1", [execution])?;
    Ok(thread.flatten().map(Mark::Post))
}

/// What `mark` shows the agent: the thread its post goes to.
fn shown(db: &Connection, mark: &Mark) -> Result<ChatMark> {
    let Mark::Post(id) = mark;
    Ok(ChatMark::Post {
        thread: crate::daemon::threads::number_of(db, *id)?,
    })
}

impl Manager {
    /// The execution whose turn the agent of `space` that `caller` runs as
    /// marks, refusing an execution that runs no agent of the space, or one
    /// without Shoal's hook that ends its turns.
    async fn marked_turn(&self, caller: Option<&Caller>, space: &Space) -> Result<ExecutionId> {
        let execution = caller.and_then(Caller::execution_id);
        let (execution, agent) = self.agent_turn(space, execution).await?;
        let settings = self.space_settings(space).await?;
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
    /// agent's turn ends either way, so a failure is only logged.
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
        match self.store.write(move |tx| take(tx, &turn)).await? {
            None => Ok(()),
            Some(Mark::Post(thread)) => self.post_marked(&space, thread, message).await,
        }
    }

    /// The user replied `prompt` in the conversation of the agent
    /// `execution` runs in the space `selector` names: record their
    /// interaction with it, drop the mark of a turn underway, and record
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
                    take(tx, turn)?;
                }
                crate::daemon::threads::record_chat_replies(tx, &space.id, prompt.as_deref())
            })
            .await?;
        self.announce_threads(delivered).await;
        Ok(())
    }
}

#[cfg(test)]
mod tests;
