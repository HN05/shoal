//! Questions an agent asks the user in its conversation: it marks the turn,
//! and the turn's final message reaches the user's questions too, until the
//! user answers there or replies in the conversation. A workspace's agent
//! marks the turn of the execution it runs, and a chat's agent the chat's.
use anyhow::{Context, Result, ensure};

use super::Manager;
use crate::{
    daemon::questions::{Turn, set_chat_mark},
    model::{Chat, PresentSpace, Workspace},
};

impl Manager {
    /// Mark the current turn of the agent of the space `selector` names, a
    /// chat's or the one `execution` runs in a workspace, as asking the user
    /// in its conversation, which needs Shoal's hook that ends the agent's
    /// turns.
    pub(crate) async fn mark_chat_question(
        &self,
        selector: &str,
        execution: Option<&str>,
    ) -> Result<()> {
        let (turn, agent, settings) = match self.present_space(selector).await? {
            PresentSpace::Workspace(workspace) => {
                self.workspace_turn(&workspace, execution).await?
            }
            PresentSpace::Chat(chat) => self.chat_turn(&chat).await?,
        };
        ensure!(
            settings.integration(&agent).reports_turn_ends(),
            "agent {agent} has no Shoal hook that ends its turns; ask with shoal questions ask \"<decision>\" --recommend <answer>"
        );
        self.store
            .run(move |db| set_chat_mark(db, &turn, true))
            .await?;
        Ok(())
    }

    /// The turn of the agent `execution` runs in `workspace`, with the agent
    /// and the settings that say whether its turns end through Shoal's hook.
    async fn workspace_turn(
        &self,
        workspace: &Workspace,
        execution: Option<&str>,
    ) -> Result<(Turn, String, crate::config::Effective)> {
        let refusal = || {
            format!(
                "only an agent Shoal runs in workspace {} asks in its chat",
                workspace.name
            )
        };
        let execution = execution.with_context(refusal)?.to_owned();
        let agent = self
            .executions
            .with_agent(&execution, |agent| {
                (agent.workspace_id == workspace.id).then(|| agent.name.clone())
            })
            .await
            .flatten()
            .with_context(refusal)?;
        let settings = self.workspace_settings(workspace).await?;
        Ok((Turn::Execution(execution), agent, settings))
    }

    /// The turn of `chat`'s agent, with the agent and the settings of its
    /// repository.
    async fn chat_turn(&self, chat: &Chat) -> Result<(Turn, String, crate::config::Effective)> {
        let settings = self.repository_settings(&chat.repository_id).await?;
        Ok((Turn::Chat(chat.id.clone()), chat.agent.clone(), settings))
    }

    /// Ask the user the question in `final_message` when the agent of the
    /// space `selector` names, a chat's or the one `execution` runs, marked
    /// the turn that ended as asking one. The agent's turn ends either way,
    /// so a failure is only logged.
    pub(crate) async fn ask_marked_chat_question(
        &self,
        selector: &str,
        execution: Option<&str>,
        final_message: Option<String>,
    ) {
        let asked = async {
            let Some(turn) = self.marked_turn(selector, execution).await? else {
                return Ok(None);
            };
            self.ask_in_chat(selector, turn, final_message.as_deref())
                .await
        };
        if let Err(error) = asked.await {
            crate::daemon::log!("question {selector} asked in its chat not recorded: {error:#}");
        }
    }

    /// The user replied in the conversation of the agent of the space
    /// `selector` names, a chat's or the one `execution` runs: close the
    /// questions the space's agents asked in their conversations, and the
    /// mark of a turn underway, and return the closed questions' numbers.
    pub(crate) async fn reply_in_chat(
        &self,
        selector: &str,
        execution: Option<&str>,
    ) -> Result<Vec<i64>> {
        let turn = self.marked_turn(selector, execution).await?;
        self.close_chat_questions(selector, turn).await
    }

    /// The turn that may carry a mark: the chat's for a chat, else the one
    /// of the agent `execution` runs, if any.
    async fn marked_turn(&self, selector: &str, execution: Option<&str>) -> Result<Option<Turn>> {
        Ok(match self.present_space(selector).await? {
            PresentSpace::Chat(chat) => Some(Turn::Chat(chat.id)),
            PresentSpace::Workspace(_) => execution.map(|id| Turn::Execution(id.to_owned())),
        })
    }
}

#[cfg(test)]
mod tests;
