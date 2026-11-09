//! Removing a chat through the removal path every space takes: the spaces
//! below it go first, whatever keeps them, then its agent and its record.
//! Unattended removals follow a workspace's rules, where the chat meets
//! every condition on a worktree.
use crate::client::space_ids::SpaceId;
use anyhow::{Context as _, Result, bail, ensure};

use super::{Removal, Retention};
use crate::client::space_names::{SpaceName, SpaceSelector};
use crate::{
    client::output::chat::RemovedChat,
    daemon::{
        cleanup::{Rule, Snapshot},
        events::EventCause,
        workspace::Manager,
    },
    model::{Chat, CleanupReason, Completion},
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
        selector: &SpaceSelector,
        children: Option<Vec<SpaceId>>,
    ) -> Result<RemovedChat> {
        let chat = self.chat(selector).await?;
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
            taking: None,
        };
        let children = self.remove_chat_for(&chat, removal).await?;
        Ok(RemovedChat { chat, children })
    }

    /// What idle or merged cleanup, by `rule`, must see unchanged before
    /// removing `chat`, or `None` when the rule does not take it.
    pub(crate) async fn chat_cleanup_snapshot(
        &self,
        chat: &Chat,
        rule: Rule,
    ) -> Result<Option<Snapshot>> {
        Ok(self.chat_cleanup_candidacy(chat, rule).await?.ok())
    }

    /// [`Self::chat_cleanup_snapshot`], or why `rule` does not take `chat`:
    /// its agent runs or waits to, something keeps it, or for idle cleanup,
    /// Shoal saved its session, as saved agents keep a workspace, or it has
    /// assignment state.
    pub(crate) async fn chat_cleanup_candidacy(
        &self,
        chat: &Chat,
        rule: Rule,
    ) -> Result<Result<Snapshot, CleanupReason>> {
        if chat.restore.is_none() {
            return Ok(Err(CleanupReason::AgentRunning));
        }
        if let Some(retention) = self.chat_keep_reason(chat).await? {
            return Ok(Err(retention.into()));
        }
        if rule == Rule::Idle {
            if self.chat_saved(&chat.id).await? {
                return Ok(Err(CleanupReason::Recovering));
            }
            if let Some(reason) = self.chat_assignment_reason(chat).await? {
                return Ok(Err(reason));
            }
        }
        // When its agent stopped, so a restore and a new stop between two
        // passes start the idle period over.
        let stopped_at = self.chat_stopped_at(&chat.id).await?.unwrap_or_default();
        let activity = self.usage.activity(&chat.id);
        Ok(Ok(Snapshot {
            rule,
            signals: crate::daemon::cleanup::signals(&stopped_at.to_string(), activity),
            tree: 0,
        }))
    }

    /// The assignment state of `chat` whose own cleanup decides its removal:
    /// a completed ancestor's or its own completion, or a PR or issue it
    /// watches.
    async fn chat_assignment_reason(&self, chat: &Chat) -> Result<Option<CleanupReason>> {
        Ok(
            if let Some(ancestor) = self.completed_ancestor(&chat.id).await? {
                Some(CleanupReason::AncestorCompleted {
                    workspace: ancestor.name().clone(),
                })
            } else if self.pr_registration(&chat.id).await?.is_some() {
                Some(CleanupReason::PrWatch)
            } else if let Some(completion) = &chat.completion {
                Some(CleanupReason::Completed {
                    integrated: completion.integrated,
                })
            } else if self.issue_registration(&chat.id).await?.is_some() {
                Some(CleanupReason::Issue)
            } else {
                None
            },
        )
    }

    /// The snapshot of `chat` as a candidate of idle cleanup, else of merged
    /// cleanup, which a request to clean up takes without waiting, or why
    /// idle cleanup keeps it.
    pub(crate) async fn requested_chat_candidacy(
        &self,
        chat: &Chat,
    ) -> Result<Result<Snapshot, CleanupReason>> {
        match self.chat_cleanup_candidacy(chat, Rule::Idle).await? {
            Ok(snapshot) => Ok(Ok(snapshot)),
            Err(reason) => Ok(self
                .chat_cleanup_snapshot(chat, Rule::Merged)
                .await?
                .ok_or(reason)),
        }
    }

    /// Remove `chat`, which idle or merged cleanup found unchanged as
    /// `snapshot`, once its grace period ends; `Ok(false)` while it runs.
    pub(crate) async fn remove_unused_chat(
        &self,
        chat: &Chat,
        snapshot: Snapshot,
        cause: EventCause,
    ) -> Result<bool> {
        // Under the PR gate link changes take, as a workspace's cleanup, so
        // no link appears while the spaces below the chat go.
        let _gate = self.gates.lock_prs([&chat.id]).await;
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

    /// Remove `chat` below `ancestor`, whose completion with cleanup stands,
    /// stopping its agent, once its grace period ends; `Ok(false)` while it
    /// runs.
    pub(crate) async fn remove_chat_under_completed(
        &self,
        chat: &Chat,
        ancestor: &super::super::done::CompletedAncestor,
    ) -> Result<bool> {
        // The sweep holds the PR gates of the chat and the ancestor; a chat
        // ancestor's spaces are held too, as for a workspace below it.
        let _held = match ancestor {
            super::super::done::CompletedAncestor::Chat(above) => {
                Some(self.chat_owners.reserve(&above.id)?)
            }
            super::super::done::CompletedAncestor::Workspace(_) => None,
        };
        (self.remove_chat_after_grace(chat, Removal::UnderCompleted { ancestor })).await
    }

    /// Remove `chat` for `removal` unless its grace period still runs. The
    /// first attempt that passes the removal's checks starts the period.
    async fn remove_chat_after_grace(&self, chat: &Chat, removal: Removal<'_>) -> Result<bool> {
        if removal.waits_for_grace() {
            self.check_chat_removal(chat, removal).await?;
            let settings = self.chat_settings(chat).await?;
            let cleanup_at = self.claim_grace(&chat.id, settings.cleanup.grace()).await?;
            if crate::time::UnixTime::now() < cleanup_at {
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
    ) -> Result<Vec<SpaceName>> {
        // Reserved under the lock a restore holds, and checked again, so no
        // restore runs it while the spaces below it go.
        let reservation = {
            let _saving = self.chat_saves.lock().await;
            let reservation = self.chat_removals.reserve(&chat.id)?;
            self.recheck_chat(&chat.id, removal).await?;
            reservation
        };
        let below = self.children(&chat.id).await?;
        let children = self.remove_children(&chat.name, below, removal).await?;
        // A save or restore under way would run the agent the removal stops.
        let _saving = self.chat_saves.lock().await;
        let chat = self.recheck_chat(&chat.id, removal).await?;
        if chat.restore.is_none() {
            self.stop_chat(&chat.id).await?;
        }
        self.ensure_ended(&chat, std::process::id()).await?;
        self.delete_chat(&chat).await?;
        drop(reservation);
        Ok(children)
    }

    /// What keeps `chat` from every unattended removal: what keeps any space,
    /// or a restore that waits to run its agent, as a queued agent keeps a
    /// workspace.
    pub(crate) async fn chat_keep_reason(&self, chat: &Chat) -> Result<Option<Retention>> {
        Ok(match self.keep_reason(&chat.id).await? {
            Some(retention) => Some(retention),
            None => {
                (chat.restore == Some(ChatRestore::Automatic)).then_some(Retention::QueuedAgent)
            }
        })
    }

    /// The chat `id` as recorded now, once `removal` still applies to it.
    async fn recheck_chat(&self, id: &SpaceId, removal: Removal<'_>) -> Result<Chat> {
        let chat = self
            .chat_by_id(id)
            .await?
            .context("the chat was removed meanwhile")?;
        self.check_chat_removal(&chat, removal).await?;
        Ok(chat)
    }

    /// Refuse `removal` of `chat` as it is now: an unattended one while
    /// something keeps it, or a directory below it stalled; any while a
    /// stopped chat's processes run on; idle or merged cleanup once it changed from the
    /// snapshot it took; completion cleanup once the completion changed.
    async fn check_chat_removal(&self, chat: &Chat, removal: Removal<'_>) -> Result<()> {
        if removal.unattended()
            && let Some(retention) = self.chat_keep_reason(chat).await?
        {
            bail!("{retention}; retaining the chat");
        }
        // Processes a stopped chat left running, and a stalled directory
        // below it that the sweep skips, refuse its removal, so neither
        // starts a grace period nor lets a space below it go.
        if chat.restore.is_some() {
            self.ensure_ended(chat, std::process::id()).await?;
        }
        let below = self.children(&chat.id).await?;
        self.ensure_none_stalled_below(below, removal).await?;
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
            // The spaces below go first, under the same completion.
            Removal::UnderCompleted { ancestor } => {
                ensure!(
                    self.children(&chat.id).await?.is_empty(),
                    "spaces below it remain; retaining it until they go"
                );
                let nearest = self.completed_ancestor(&chat.id).await?;
                ensure!(
                    nearest.is_some_and(|nearest| nearest.id() == ancestor.id()),
                    "{} withdrew its completion, changed or no longer has the chat below it; retaining the chat",
                    ancestor.name()
                );
            }
            _ => {}
        }
        Ok(())
    }
}
