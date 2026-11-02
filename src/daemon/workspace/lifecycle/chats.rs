//! Removing a chat through the removal path every space takes: the spaces
//! below it go first, whatever keeps them, then its agent and its record.
//! Unattended removals follow a workspace's rules, where the chat meets
//! every condition on a worktree.
use anyhow::{Context as _, Result, bail, ensure};

use super::{Removal, Retention};
use crate::{
    client::output::chat::RemovedChat,
    daemon::{
        cleanup::{Rule, Snapshot},
        events::EventCause,
        workspace::Manager,
    },
    model::{Chat, Completion},
    removal::{InspectionPolicy, UnsavedWork},
    state::ChatRestore,
};

impl Manager {
    /// Remove the chat `selector` names, by ID or name, at the user's
    /// request, stopping its agent: first the spaces below it, of which
    /// `children`, when given, are the IDs the user saw and the only ones it
    /// takes. Returns the chat and the names of the spaces it took.
    pub async fn remove_chat(
        &self,
        selector: &str,
        children: Option<Vec<String>>,
    ) -> Result<RemovedChat> {
        let chat = self
            .recorded_chat(selector)
            .await?
            .with_context(|| format!("no chat {selector}"))?;
        let unsaved = UnsavedWork::Confirmed {
            files: false,
            detached_head: None,
            files_restorable: false,
            children,
        };
        let removal = Removal::Manual {
            unsaved: &unsaved,
            inspection: InspectionPolicy::GitOnly,
            release_holds: true,
        };
        let children = self.remove_chat_for(&chat, removal).await?;
        Ok(RemovedChat { chat, children })
    }

    /// What idle or merged cleanup, by `rule`, must see unchanged before
    /// removing `chat`, or `None` when the rule does not take it: its agent
    /// stopped and waits for no restore, nothing keeps it, and for idle
    /// cleanup, it has no assignment state.
    pub(crate) async fn chat_cleanup_snapshot(
        &self,
        chat: &Chat,
        rule: Rule,
    ) -> Result<Option<Snapshot>> {
        if chat.restore != Some(ChatRestore::Resume) || self.keep_reason(&chat.id).await?.is_some()
        {
            return Ok(None);
        }
        if rule == Rule::Idle && self.chat_assigned(chat).await? {
            return Ok(None);
        }
        Ok(Some(Snapshot {
            rule,
            signals: crate::daemon::cleanup::signals("", self.usage.activity(&chat.id)),
            tree: 0,
        }))
    }

    /// Whether `chat` has assignment state, whose own cleanup decides its
    /// removal: a completion, or a PR or issue it watches.
    async fn chat_assigned(&self, chat: &Chat) -> Result<bool> {
        Ok(chat.completion.is_some()
            || self.pr_registration(&chat.id).await?.is_some()
            || self.issue_registration(&chat.id).await?.is_some())
    }

    /// Remove `chat`, which idle or merged cleanup found unchanged as
    /// `snapshot`, once its grace period ends; `Ok(false)` while it runs.
    pub(crate) async fn remove_unused_chat(
        &self,
        chat: &Chat,
        snapshot: Snapshot,
        cause: EventCause,
    ) -> Result<bool> {
        (self.remove_chat_after_grace(chat, Removal::Automatic { snapshot, cause })).await
    }

    /// Remove `chat` for its `completion` with cleanup, stopping its agent,
    /// once its grace period ends; `Ok(false)` while it runs.
    pub(crate) async fn remove_completed_chat(
        &self,
        chat: &Chat,
        completion: &Completion,
    ) -> Result<bool> {
        let removal = Removal::Completed {
            head: &completion.head,
            integrated: completion.integrated,
            cause: EventCause::Completion,
        };
        self.remove_chat_after_grace(chat, removal).await
    }

    /// Remove `chat` for `removal` unless its grace period still runs. The
    /// first attempt that passes the removal's checks starts the period.
    async fn remove_chat_after_grace(&self, chat: &Chat, removal: Removal<'_>) -> Result<bool> {
        if removal.waits_for_grace() {
            self.check_chat_removal(chat, removal).await?;
            let settings = self.repository_settings(&chat.repository_id).await?;
            let cleanup_at = self.claim_grace(&chat.id, settings.cleanup.grace()).await?;
            if crate::time::unix_seconds() < cleanup_at {
                return Ok(false);
            }
        }
        self.remove_chat_for(chat, removal).await.map(|_| true)
    }

    /// Remove `chat` for `removal`: the spaces below it, through the path
    /// that removes them below any space, then, once the removal's checks
    /// still pass, its agent and its record. Returns the names of the spaces
    /// below it it took, deepest first.
    pub(super) async fn remove_chat_for(
        &self,
        chat: &Chat,
        removal: Removal<'_>,
    ) -> Result<Vec<String>> {
        self.check_chat_removal(chat, removal).await?;
        let below = self.chat_children(&chat.id).await?;
        self.ensure_none_stalled_below(below.clone(), removal)
            .await?;
        let children = self.remove_children(&chat.name, below, removal).await?;
        // A save or restore under way would run the agent the removal stops.
        let _saving = self.chat_saves.lock().await;
        let chat = self
            .recorded_chat(&chat.id)
            .await?
            .context("the chat was removed meanwhile")?;
        self.check_chat_removal(&chat, removal).await?;
        if chat.restore.is_none() {
            self.stop_chat(&chat.id).await?;
        }
        self.ensure_ended(&chat, std::process::id()).await?;
        self.delete_chat(&chat).await?;
        Ok(children)
    }

    /// Refuse `removal` of `chat` as it is now: an unattended one while
    /// something keeps it; idle or merged cleanup once it changed from the
    /// snapshot it took; completion cleanup once the completion changed.
    async fn check_chat_removal(&self, chat: &Chat, removal: Removal<'_>) -> Result<()> {
        if removal.unattended() {
            let retention = match self.keep_reason(&chat.id).await? {
                Some(retention) => Some(retention),
                // A restore that waits to run its agent keeps it, as a
                // queued agent keeps a workspace.
                None => {
                    (chat.restore == Some(ChatRestore::Automatic)).then_some(Retention::QueuedAgent)
                }
            };
            if let Some(retention) = retention {
                bail!("{retention}; retaining the chat");
            }
        }
        match removal {
            Removal::Automatic { snapshot, .. } => ensure!(
                self.chat_cleanup_snapshot(chat, snapshot.rule).await? == Some(snapshot),
                "the chat changed before removal; retaining it"
            ),
            Removal::Completed { head, .. } => ensure!(
                (chat.completion.as_ref())
                    .is_some_and(|completion| completion.cleanup && completion.head == head),
                "the chat withdrew its completion or changed since; retaining it"
            ),
            _ => {}
        }
        Ok(())
    }
}
