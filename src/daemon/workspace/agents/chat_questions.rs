//! Questions an agent asks the user in its chat: it marks the turn, and the
//! turn's final message reaches the user's questions too, until the user
//! answers there or replies in the chat.
use anyhow::{Context, Result, ensure};

use super::Manager;
use crate::daemon::questions::set_chat_mark;

impl Manager {
    /// Mark the current turn of the agent `execution` runs in the workspace
    /// `selector` names as asking the user in its chat, which needs Shoal's
    /// hook that ends the agent's turns.
    pub(crate) async fn mark_chat_question(
        &self,
        selector: &str,
        execution: Option<&str>,
    ) -> Result<()> {
        let workspace = self.workspace(selector).await?;
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
        let settings = self.workspace_settings(&workspace).await?;
        ensure!(
            settings.integration(&agent).reports_turn_ends(),
            "agent {agent} has no Shoal hook that ends its turns; ask with shoal questions ask \"<decision>\" --recommend <answer>"
        );
        self.store
            .run(move |db| set_chat_mark(db, &execution, true))
            .await?;
        Ok(())
    }

    /// Ask the user the question in `final_message` when the agent
    /// `execution` runs marked the turn that ended as asking one. The
    /// agent's turn ends either way, so a failure is only logged.
    pub(crate) async fn ask_marked_chat_question(
        &self,
        selector: &str,
        execution: Option<&str>,
        final_message: Option<String>,
    ) {
        let Some(execution) = execution else {
            return;
        };
        let asked = self
            .ask_in_chat(selector, execution, final_message.as_deref())
            .await;
        if let Err(error) = asked {
            crate::daemon::log!(
                "question execution {execution} asked in its chat not recorded: {error:#}"
            );
        }
    }

    /// The user replied in the chat of the agent `execution` runs: close the
    /// questions the workspace's agents asked in their chats, and the mark
    /// of a turn underway, and return the closed questions' numbers.
    pub(crate) async fn reply_in_chat(
        &self,
        selector: &str,
        execution: Option<&str>,
    ) -> Result<Vec<i64>> {
        self.close_chat_questions(selector, execution.map(str::to_owned))
            .await
    }
}

#[cfg(test)]
mod tests;
