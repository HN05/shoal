//! The agents of chats from before a chat's agent ran as an execution of
//! the chat, which the upgrade stopped: each is saved as a workspace's
//! stopped agent is, for `shoal resume` or, when Shoal saved it to come back
//! on its own, for the agent queue, and one that exited keeps its session.
use anyhow::{Context as _, Result};
use rusqlite::Connection;

use crate::client::execution_ids::ExecutionId;
use crate::client::space_ids::SpaceId;
use crate::{
    daemon::{log, workspace::Manager},
    execution::{
        TrackedAgent,
        recovery::{self, LastSession, LastSessionExt, Record, RecordExt, Stage},
    },
    model::Chat,
    state::AgentState,
};

/// What the upgrade kept of a chat's agent.
struct Upgraded {
    chat: SpaceId,
    /// Shoal stopped it, or the upgrade did, rather than it exiting.
    saved: bool,
    /// Shoal saved it to come back on its own.
    automatic: bool,
    /// The space whose process stopped it.
    stopped_by: Option<SpaceId>,
    /// What it was doing when it stopped.
    turn: Option<AgentState>,
}

impl Manager {
    /// Save the agent of each chat the upgrade stopped, then forget what the
    /// upgrade kept of it. One that cannot be saved stays for the next
    /// start.
    pub(crate) async fn save_upgraded_chat_agents(&self) -> Result<()> {
        for upgraded in self.store.run(upgraded).await? {
            if let Err(error) = self.save_upgraded(&upgraded).await {
                log!("agent of chat {} not saved: {error:#}", upgraded.chat);
                continue;
            }
            let chat = upgraded.chat.clone();
            (self.store)
                .write(move |tx| {
                    tx.execute("DELETE FROM upgraded_chat_agents WHERE chat_id=?1", [chat])?;
                    Ok(())
                })
                .await?;
        }
        Ok(())
    }

    /// Save the agent of the chat `upgraded` names under an execution of
    /// its own, if the chat is still there.
    async fn save_upgraded(&self, upgraded: &Upgraded) -> Result<()> {
        let Some(chat) = self.chat_by_id(&upgraded.chat).await? else {
            return Ok(());
        };
        let execution = ExecutionId::generate();
        if !upgraded.saved {
            if self.completion(&chat.id).await?.is_none() {
                exited(&chat, &execution).write(&self.paths, &chat.id)?;
            }
            return Ok(());
        }
        let restore = (upgraded.automatic)
            .then(|| crate::terminal::detached_start_environment(chat.origin.as_ref()));
        let record = stopped(&chat, upgraded);
        recovery::save_stopped(&self.paths, &chat.id, &execution, record, restore)
            .map(drop)
            .context("save its stopped agent")
    }
}

/// The record of the agent of `chat` that Shoal or the upgrade stopped.
fn stopped(chat: &Chat, upgraded: &Upgraded) -> Record {
    let agent = TrackedAgent {
        name: chat.agent.clone(),
        model: Default::default(),
        mode: chat.mode.clone(),
        narrowing: chat.narrowing,
    };
    Record {
        session: chat.session.clone(),
        stopped_by: upgraded.stopped_by.clone(),
        turn: upgraded.turn,
        ..Record::new(&agent, None, Stage::Stopped, chat.origin.clone())
    }
}

/// The last session of the agent of `chat`, which exited, as `execution`.
fn exited(chat: &Chat, execution: &ExecutionId) -> LastSession {
    LastSession {
        execution: execution.clone(),
        agent: chat.agent.clone(),
        session: chat.session.clone(),
        moved: false,
        copied: None,
        converted: false,
        origin: chat.origin.clone(),
        stopped_by: None,
        stop: None,
        model: Default::default(),
        mode: chat.mode.clone(),
        narrowing: chat.narrowing,
    }
}

/// What the upgrade kept of each chat's agent.
fn upgraded(db: &mut Connection) -> Result<Vec<Upgraded>> {
    let mut statement =
        db.prepare("SELECT chat_id,saved,automatic,stopped_by,turn FROM upgraded_chat_agents")?;
    let rows = statement.query_map([], |row| {
        Ok(Upgraded {
            chat: row.get(0)?,
            saved: row.get(1)?,
            automatic: row.get(2)?,
            stopped_by: row.get(3)?,
            turn: row.get(4)?,
        })
    })?;
    Ok(rows.collect::<rusqlite::Result<_>>()?)
}

#[cfg(test)]
mod tests;
