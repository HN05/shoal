//! A chat's `shoal done`: it records the chat's work as finished, while its
//! agent and session keep running until completion cleanup, when requested,
//! removes the chat as it removes a workspace.
use anyhow::{Context as _, Result, ensure};
use rusqlite::{Connection, OptionalExtension, params};

use crate::{
    client::assignments::Done,
    daemon::{
        cleanup::SpaceFailures,
        notifications::{About, NotificationKind},
        questions,
        spaces::removal::Cleanup,
        workspace::{
            Manager,
            done::{CompletedAncestor, push_completion_details},
        },
        workspace_files::Attacher,
    },
    model::{Chat, Completion, PresentSpace},
};

impl Manager {
    /// Record `done` for the running chat `selector` names at its
    /// checkout's HEAD, with copies of the files it hands back attached by
    /// `attacher`; its open questions close, as a finished workspace's do.
    pub async fn mark_chat_done(
        &self,
        selector: &str,
        done: Done,
        attacher: &Attacher,
    ) -> Result<Completion> {
        done.check()?;
        ensure!(
            !done.integrated,
            "a chat has no branch; integrated applies to workspaces"
        );
        let chat = self.done_chat(selector).await?;
        let _guard = self.gates.lock_prs([&chat.id]).await;
        let settings = self.chat_settings(&chat).await?;
        let head = self.chat_head(&chat).await?;
        let space = PresentSpace::Chat(chat.clone()).into();
        let results = self.keep_results(&space, done.results, attacher).await?;
        let completion = Completion {
            head,
            cleanup: done.cleanup || settings.cleanup.done.enabled,
            integrated: false,
            results,
            summary: done.summary,
            error: None,
        };
        let (id, recorded) = (chat.id.clone(), completion.clone());
        let closed = (self.store)
            .run(move |db| record_completion(db, &id, &recorded))
            .await?;
        if !closed.is_empty() {
            self.signals.question_changed();
        }
        let mut notice = String::from(match completion.cleanup {
            true => "chat finished; cleanup requested",
            false => "chat finished; no cleanup requested",
        });
        push_completion_details(&mut notice, &chat.name, &completion, &closed);
        let about = About::space(&chat.name, &chat.id);
        (self.notify_about(about, NotificationKind::SpaceDone, notice)).await;
        self.signals.wake_cleanup();
        Ok(completion)
    }

    /// Remove each chat whose completion requested cleanup once its grace
    /// period ends, unless something keeps it. A PR it watches does not: a
    /// chat has no branch whose work an unmerged PR would keep. A failure in
    /// one chat does not skip the others.
    pub(crate) async fn sweep_completed_chats(&self, failed: &mut SpaceFailures) -> Result<()> {
        for listed in self.chats().await? {
            if let Err(error) = self.sweep_completed_chat(&listed).await {
                failed.push(&listed.id, &listed.name, &error);
            }
        }
        Ok(())
    }

    /// Remove `listed` for its own completion or a completed ancestor's,
    /// under its PR gate, which a completion changes under.
    async fn sweep_completed_chat(&self, listed: &Chat) -> Result<()> {
        // An ancestor's completion that takes the chat changes under the
        // ancestor's gate, so that one is held too.
        let gated = self.completed_ancestor(&listed.id).await?;
        let gates = std::iter::once(listed.id.as_str()).chain(gated.as_ref().map(|a| a.id()));
        let _guard = self.gates.lock_prs(gates).await;
        let Some(chat) = self.chat_by_id(&listed.id).await? else {
            return Ok(());
        };
        if let Some(ancestor) = self.completed_ancestor(&chat.id).await? {
            // Another ancestor's completion waits for the next sweep, which
            // holds its gate.
            if gated.is_some_and(|gated| gated.id() == ancestor.id()) {
                self.sweep_chat_under_completed(&chat, &ancestor).await?;
            }
            return Ok(());
        }
        let Some(completion) = chat.completion.clone() else {
            return Ok(());
        };
        if !completion.cleanup || self.chat_keep_reason(&chat).await?.is_some() {
            return Ok(());
        }
        let removal = self.remove_completed_chat(&chat, &completion).await;
        if matches!(removal, Ok(false)) {
            return Ok(());
        }
        (self.announce_removal(&chat, Cleanup::Completion, &removal)).await
    }

    /// Remove `chat` for its completed `ancestor` once its grace period
    /// ends, unless something keeps it or spaces below it remain, which go
    /// first under the same completion.
    async fn sweep_chat_under_completed(
        &self,
        chat: &Chat,
        ancestor: &CompletedAncestor,
    ) -> Result<()> {
        if !self.children(&chat.id).await?.is_empty()
            || self.chat_keep_reason(chat).await?.is_some()
        {
            return Ok(());
        }
        let removal = self.remove_chat_under_completed(chat, ancestor).await;
        if matches!(removal, Ok(false)) {
            return Ok(());
        }
        let cleanup = Cleanup::AncestorCompletion(ancestor.name());
        self.announce_removal(chat, cleanup, &removal).await
    }

    /// Withdraw the completion the chat `id` recorded.
    pub async fn undo_chat_done(&self, id: &str) -> Result<Completion> {
        // A stopped chat withdraws it too, so completion cleanup leaves it.
        let chat = (self.chat_by_id(id).await?).with_context(|| format!("no chat {id}"))?;
        let _guard = self.gates.lock_prs([&chat.id]).await;
        let id = chat.id.clone();
        self.store.run(move |db| take_completion(db, &id)).await
    }

    async fn done_chat(&self, id: &str) -> Result<Chat> {
        (self.running_chat_by_id(id).await?).with_context(|| format!("no running chat {id}"))
    }

    /// The HEAD of `chat`'s checkout, which its completion records; empty
    /// for a chat without a repository, which has no checkout.
    async fn chat_head(&self, chat: &Chat) -> Result<String> {
        let Some(checkout) = self.chat_repository(chat).await? else {
            return Ok(String::new());
        };
        let (head, _) = crate::git::head_and_branch(&checkout.path)
            .await
            .context("cannot read the checkout's HEAD")?;
        Ok(head)
    }
}

/// Record `completion` for the running chat `id` and close its open
/// questions, which its finished work waits on no longer; the questions it
/// closed. A completion that would change nothing fails.
fn record_completion(
    db: &mut Connection,
    id: &str,
    completion: &Completion,
) -> Result<Vec<crate::daemon::item_numbers::ItemNumber>> {
    let tx = db.transaction()?;
    let previous: Option<Option<String>> = tx
        .query_row(
            "SELECT completion FROM chats WHERE id=?1 AND restore IS NULL",
            [id],
            |row| row.get(0),
        )
        .optional()?;
    let previous = previous.context("the chat stopped running")?;
    let previous: Option<Completion> = previous
        .map(|record| serde_json::from_str(&record))
        .transpose()?;
    tx.execute(
        "UPDATE chats SET completion=?2 WHERE id=?1",
        params![id, serde_json::to_string(completion)?],
    )?;
    let closed = questions::close_open(&tx, id)?;
    ensure!(
        previous.as_ref() != Some(completion) || !closed.is_empty(),
        "the chat is already marked done at this HEAD"
    );
    tx.commit()?;
    Ok(closed)
}

/// Delete and return the completion the chat `id` recorded.
fn take_completion(db: &mut Connection, id: &str) -> Result<Completion> {
    let tx = db.transaction()?;
    let record: String = tx
        .query_row(
            "SELECT completion FROM chats WHERE id=?1 AND completion IS NOT NULL",
            [id],
            |row| row.get(0),
        )
        .optional()?
        .context("the chat is not marked done")?;
    // An unreadable record stays in place for diagnosis.
    let completion = serde_json::from_str(&record)?;
    tx.execute("UPDATE chats SET completion=NULL WHERE id=?1", [id])?;
    tx.commit()?;
    Ok(completion)
}

#[cfg(test)]
mod tests;
