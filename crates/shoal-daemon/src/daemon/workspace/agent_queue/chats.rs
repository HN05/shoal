//! The launches of new chats' agents: admitting one under the agent slots or
//! queuing it, and attaching it to the chat its start records.
use anyhow::{Result, ensure};
use rusqlite::params;

use super::{
    blockers,
    store::{self, Launch, insert_launch},
};
use crate::client::{queue::QueueEntryId, space_ids::SpaceId};
use crate::{
    daemon::{scope::Caller, workspace::Manager},
    model::{ChatQueueRequest, QueueAdmission},
    state::{QueueKind, QueueState},
};

impl Manager {
    /// Admit the launch of the new chat `request` describes for `caller`, or
    /// queue it, as a new workspace's launch is: one that no limit applies
    /// to starts untracked, and one admitted here starts in the caller,
    /// which records the chat and attaches the launch to it. One that waits
    /// starts from the queue, which runs its plan.
    pub(in crate::daemon) async fn queue_chat_add(
        &self,
        mut request: ChatQueueRequest,
        caller: Option<&Caller>,
    ) -> Result<QueueAdmission> {
        let repository = match &request.repository {
            Some(selector) => Some(self.repository(selector).await?.id),
            None => None,
        };
        request.priority = self.given_priority(caller, request.priority).await?;
        let _gate = self.agent_queue.lock_admission().await;
        let repositories = repository.iter().cloned().collect();
        let limits = self.agent_limits(repositories, &[], &[]).await?;
        if !limits.apply_to(repository.as_ref()) {
            return Ok(QueueAdmission {
                start: true,
                entry: None,
                waiting: 0,
            });
        }
        let id = QueueEntryId::generate();
        let now = request.now;
        let launch = Launch {
            id: id.clone(),
            repository,
            request: request.into(),
            state: match now {
                true => QueueState::Starting,
                false => QueueState::Queued,
            },
            opens: None,
            scope: caller.map(|caller| caller.space_id.clone()),
        };
        let removing = self.space_removals.clone();
        self.store
            .write(move |tx| insert_launch(tx, &launch, &removing))
            .await?;
        let admitted = now || self.admit_waiting(Some(&id)).await?;
        self.admission_of(&id, admitted).await
    }

    /// Record `chat` as the one the launch `id` starts its agent in, whether
    /// it starts or waits, unless a launch already holds the chat's agent.
    pub(super) async fn attach_chat(&self, id: &QueueEntryId, chat: &SpaceId) -> Result<()> {
        let _gate = self.agent_queue.lock_admission().await;
        let (id, chat) = (id.to_owned(), chat.to_owned());
        let attached = self
            .store
            .write(move |tx| {
                ensure!(
                    !store::queues_launch_in(tx, &chat)?,
                    "chat {chat} has an agent waiting in the agent queue"
                );
                let attached = tx.execute(
                    "UPDATE agent_queue SET space_id=?2 WHERE id=?1 AND kind=?3
                     AND state IN (?4,?5) AND space_id IS NULL AND opens_workspace_id IS NULL",
                    params![
                        id,
                        chat,
                        QueueKind::Launch,
                        QueueState::Queued,
                        QueueState::Starting
                    ],
                )?;
                if attached == 1 {
                    blockers::replace_launch(tx, &id, &chat)?;
                }
                Ok(attached)
            })
            .await?;
        ensure!(
            attached == 1,
            "queue entry no longer starts or waits, or starts its agent in another space"
        );
        Ok(())
    }
}

#[cfg(test)]
mod tests;
