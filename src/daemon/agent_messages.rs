//! Messages for the agents working in a workspace, from the user or the
//! daemon. The daemon never writes to an agent's terminal: agents receive
//! messages through `shoal messages`, watches and agent hooks, and a delivered
//! message is deleted.
use anyhow::{Result, ensure};
use rusqlite::{OptionalExtension, params};
use serde::{Deserialize, Serialize};

use crate::daemon::workspace::Manager;

/// Unread messages one workspace may hold; together they fit one response.
const MAX_UNREAD: i64 = 50;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentMessage {
    pub id: i64,
    /// Unix seconds.
    pub created_at: i64,
    pub message: String,
}

impl Manager {
    /// Queue a message for the workspace's agents. An identical unread message
    /// is not repeated.
    pub async fn send_agent_message(&self, selector: &str, message: String) -> Result<()> {
        crate::validate::message(&message)?;
        let workspace = self.workspace(selector).await?;
        self.queue_agent_message(&workspace.id, message).await
    }

    pub(crate) async fn queue_agent_message(
        &self,
        workspace_id: &str,
        message: String,
    ) -> Result<()> {
        let workspace_id = workspace_id.to_owned();
        let queued = self
            .store
            .run(move |db| {
                let tx = db.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
                let pending: Option<i64> = tx
                    .query_row(
                        "SELECT id FROM agent_messages WHERE workspace_id=?1 AND message=?2",
                        params![workspace_id, message],
                        |row| row.get(0),
                    )
                    .optional()?;
                if pending.is_some() {
                    return Ok(false);
                }
                let unread: i64 = tx.query_row(
                    "SELECT COUNT(*) FROM agent_messages WHERE workspace_id=?1",
                    [&workspace_id],
                    |row| row.get(0),
                )?;
                ensure!(
                    unread < MAX_UNREAD,
                    "the workspace already has {MAX_UNREAD} unread agent messages"
                );
                tx.execute(
                    "INSERT INTO agent_messages(workspace_id,created_at,message) VALUES (?1,?2,?3)",
                    params![
                        workspace_id,
                        i64::try_from(crate::time::unix_seconds())?,
                        message
                    ],
                )?;
                tx.commit()?;
                Ok(true)
            })
            .await?;
        if queued {
            self.agent_messages_changed.send_modify(|count| *count += 1);
        }
        Ok(())
    }

    /// Unread messages, oldest first. They stay unread until delivered.
    pub async fn agent_messages(&self, selector: &str) -> Result<Vec<AgentMessage>> {
        let workspace = self.workspace(selector).await?;
        self.store
            .run(move |db| {
                Ok(db
                    .prepare(
                        "SELECT id,created_at,message FROM agent_messages WHERE workspace_id=?1 ORDER BY id",
                    )?
                    .query_map([workspace.id], |row| {
                        Ok(AgentMessage {
                            id: row.get(0)?,
                            created_at: row.get(1)?,
                            message: row.get(2)?,
                        })
                    })?
                    .collect::<rusqlite::Result<Vec<_>>>()?)
            })
            .await
    }

    /// Delete exactly these messages once an agent has received them.
    pub async fn mark_agent_messages_delivered(&self, selector: &str, ids: Vec<i64>) -> Result<()> {
        let workspace = self.workspace(selector).await?;
        self.store
            .run(move |db| {
                let tx = db.transaction()?;
                for id in ids {
                    tx.execute(
                        "DELETE FROM agent_messages WHERE id=?1 AND workspace_id=?2",
                        params![id, workspace.id],
                    )?;
                }
                tx.commit()?;
                Ok(())
            })
            .await
    }
}

#[cfg(test)]
mod tests {
    use crate::{
        daemon::{
            scope::{self, Caller},
            workspace::Manager,
        },
        protocol::Method,
        test_support::{manager, repository},
    };

    async fn workspace(
        temp: &tempfile::TempDir,
        manager: &Manager,
        name: &str,
    ) -> crate::model::Workspace {
        let checkout = repository(temp.path(), "repo");
        let repo = manager
            .register_repository(checkout.to_str().unwrap().into(), None, None)
            .await
            .unwrap();
        manager
            .create_workspace(&repo.id, name.into(), None, None, None)
            .await
            .unwrap()
    }

    #[tokio::test]
    async fn messages_wait_until_delivered_and_repeats_collapse() {
        let (temp, manager) = manager().await;
        let workspace = workspace(&temp, &manager, "receiver").await;
        let changed = manager.agent_messages_changed.subscribe();
        for _ in 0..2 {
            manager
                .send_agent_message(&workspace.name, "Stop the dev server".into())
                .await
                .unwrap();
        }
        assert!(changed.has_changed().unwrap());
        manager
            .send_agent_message(&workspace.name, "Rebase onto main".into())
            .await
            .unwrap();
        assert!(
            manager
                .send_agent_message(&workspace.name, "two\nlines".into())
                .await
                .is_err()
        );
        let messages = manager.agent_messages(&workspace.name).await.unwrap();
        let texts: Vec<_> = messages.iter().map(|m| m.message.as_str()).collect();
        assert_eq!(texts, ["Stop the dev server", "Rebase onto main"]);
        manager
            .mark_agent_messages_delivered(&workspace.name, vec![messages[0].id])
            .await
            .unwrap();
        let remaining = manager.agent_messages(&workspace.name).await.unwrap();
        assert_eq!(remaining, messages[1..]);
        // A delivered message may be sent again.
        manager
            .send_agent_message(&workspace.name, "Stop the dev server".into())
            .await
            .unwrap();
        assert_eq!(
            manager.agent_messages(&workspace.name).await.unwrap().len(),
            2
        );
    }

    #[tokio::test]
    async fn unread_messages_are_bounded() {
        let (temp, manager) = manager().await;
        let workspace = workspace(&temp, &manager, "receiver").await;
        for n in 0..super::MAX_UNREAD {
            manager
                .send_agent_message(&workspace.name, format!("message {n}"))
                .await
                .unwrap();
        }
        assert!(
            manager
                .send_agent_message(&workspace.name, "one more".into())
                .await
                .is_err()
        );
    }

    #[tokio::test]
    async fn workspace_processes_read_their_own_messages_but_cannot_send() {
        let (temp, manager) = manager().await;
        let workspace = workspace(&temp, &manager, "receiver").await;
        let other = manager
            .create_workspace(&workspace.repository_id, "other".into(), None, None, None)
            .await
            .unwrap();
        manager
            .issue_scope(
                "scope".into(),
                Caller {
                    workspace_id: workspace.id.clone(),
                    execution: None,
                },
            )
            .await;
        for (target, allowed) in [(&workspace.name, true), (&other.name, false)] {
            for mut method in [
                Method::AgentMessages {
                    workspace: target.clone(),
                },
                Method::MarkAgentMessagesDelivered {
                    workspace: target.clone(),
                    ids: Vec::new(),
                },
            ] {
                assert_eq!(
                    scope::authorize(&manager, Some("scope"), &mut method)
                        .await
                        .is_ok(),
                    allowed
                );
            }
        }
        let mut method = Method::SendAgentMessage {
            workspace: workspace.name.clone(),
            message: "hello".into(),
        };
        assert!(
            scope::authorize(&manager, Some("scope"), &mut method)
                .await
                .is_err()
        );
    }
}
