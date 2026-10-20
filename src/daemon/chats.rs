//! Chats: agents the user talks with in a repository's registered checkout.
//! They are records, not tracked executions, so they never queue, stop under
//! pressure or resume; a chat lasts while its process lives.
use std::{sync::Arc, time::Duration};

use anyhow::{Context as _, Result};
use rusqlite::{Connection, params};
use uuid::Uuid;

use crate::{
    daemon::{log, workspace::Manager},
    model::Chat,
    process::identity::{self, Identity},
};

/// How soon a chat's end reaches the journal when nothing lists chats.
const SWEEP_INTERVAL: Duration = Duration::from_secs(5);

pub(super) async fn run(manager: Arc<Manager>) {
    loop {
        tokio::time::sleep(SWEEP_INTERVAL).await;
        let _operation = manager.background_operations.read().await;
        if let Err(error) = manager.end_finished_chats().await {
            log!("chats: {error:#}");
        }
    }
}

impl Manager {
    /// Record the chat whose agent replaces process `pid` in the
    /// repository's checkout.
    pub async fn start_chat(
        &self,
        repository: crate::forge::repository::Selector,
        agent: String,
        pid: u32,
    ) -> Result<Chat> {
        let repository = self.repository(repository).await?.id;
        let process = identity::capture(pid)?.context("the chat process is not running")?;
        self.store
            .run(move |db| {
                let tx = db.transaction()?;
                let number: u32 = tx.query_row(
                    "SELECT COALESCE(MIN(number + 1), 1) FROM (SELECT 0 AS number UNION ALL
                         SELECT number FROM chats WHERE repository_id=?1) AS used
                     WHERE number + 1 NOT IN (SELECT number FROM chats WHERE repository_id=?1)",
                    [&repository],
                    |row| row.get(0),
                )?;
                let chat = Chat {
                    id: Uuid::new_v4().to_string(),
                    repository_id: repository,
                    number,
                    agent,
                    pid: process.pid,
                    started_at: i64::try_from(crate::time::unix_seconds())?,
                };
                tx.execute(
                    "INSERT INTO chats(id,repository_id,number,agent,pid,birth,started_at)
                     VALUES (?1,?2,?3,?4,?5,?6,?7)",
                    params![
                        chat.id,
                        chat.repository_id,
                        chat.number,
                        chat.agent,
                        chat.pid,
                        process.birth,
                        chat.started_at
                    ],
                )?;
                tx.commit()?;
                Ok(chat)
            })
            .await
    }

    /// Running chats in start order, after ending those whose process exited.
    pub async fn chats(&self) -> Result<Vec<Chat>> {
        self.end_finished_chats().await?;
        self.store
            .run(|db| {
                Ok(db
                    .prepare(
                        "SELECT id,repository_id,number,agent,pid,started_at FROM chats
                         ORDER BY started_at, rowid",
                    )?
                    .query_map([], |row| {
                        Ok(Chat {
                            id: row.get(0)?,
                            repository_id: row.get(1)?,
                            number: row.get(2)?,
                            agent: row.get(3)?,
                            pid: row.get(4)?,
                            started_at: row.get(5)?,
                        })
                    })?
                    .collect::<rusqlite::Result<_>>()?)
            })
            .await
    }

    /// Delete chats whose process has exited, which returns their workspaces
    /// to the user. A process that cannot be read keeps its chat.
    pub(crate) async fn end_finished_chats(&self) -> Result<()> {
        let recorded = self.store.run(recorded_processes).await?;
        let mut ended = Vec::new();
        for (id, process) in recorded {
            match identity::capture(process.pid) {
                Ok(current) if current.as_ref() != Some(&process) => ended.push(id),
                Ok(_) => {}
                Err(error) => log!("chat {id}: {error:#}"),
            }
        }
        if ended.is_empty() {
            return Ok(());
        }
        self.store
            .run(move |db| {
                let tx = db.transaction()?;
                for id in &ended {
                    tx.execute("DELETE FROM chats WHERE id=?1", [id])?;
                }
                tx.commit()?;
                Ok(())
            })
            .await
    }

    /// Give `workspace` to the running chat that opened it, or to the user
    /// when it was opened outside one or the chat has ended.
    pub async fn assign_chat(&self, workspace: &str, chat: Option<String>) -> Result<()> {
        let workspace = workspace.to_owned();
        self.store
            .run(move |db| {
                db.execute(
                    "UPDATE workspaces SET chat_id=(SELECT id FROM chats WHERE id=?2) WHERE id=?1",
                    params![workspace, chat],
                )?;
                Ok(())
            })
            .await
    }
}

fn recorded_processes(db: &mut Connection) -> Result<Vec<(String, Identity)>> {
    Ok(db
        .prepare("SELECT id,pid,birth FROM chats")?
        .query_map([], |row| {
            Ok((
                row.get(0)?,
                Identity {
                    pid: row.get(1)?,
                    birth: row.get(2)?,
                },
            ))
        })?
        .collect::<rusqlite::Result<_>>()?)
}

#[cfg(test)]
mod tests;
