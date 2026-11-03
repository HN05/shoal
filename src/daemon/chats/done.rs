//! A chat's `shoal done`: it records the chat's work as finished, while its
//! agent and session keep running until completion cleanup, when requested,
//! removes the chat as it removes a workspace.
use anyhow::{Context as _, Result, ensure};
use rusqlite::{Connection, OptionalExtension, params};

use crate::{
    daemon::{
        cleanup::{SpaceFailures, notify_chat_removal},
        notifications::{About, NotificationKind},
        questions,
        workspace::{Manager, done::push_closed_and_results},
        workspace_files::{self, Attacher, NewFile},
    },
    model::{Chat, Completion, PresentSpace},
};

impl Manager {
    /// Record the running chat `selector` names as done at its checkout's
    /// HEAD, with copies of the files `results` handed back, attached by
    /// `attacher`; its open questions close, as a finished workspace's do.
    /// `cleanup` requests cleanup even when `[cleanup.done] enabled` is
    /// false.
    pub async fn mark_chat_done(
        &self,
        selector: &str,
        cleanup: bool,
        results: Vec<NewFile>,
        attacher: &Attacher,
    ) -> Result<Completion> {
        workspace_files::check_results(&results)?;
        let chat = self.done_chat(selector).await?;
        let _guard = self.gates.lock_prs([&chat.id]).await;
        let settings = self.repository_settings(&chat.repository_id).await?;
        let checkout = self.repository(&chat.repository_id).await?;
        let (head, _) = crate::git::head_and_branch(&checkout.path)
            .await
            .context("cannot read the checkout's HEAD")?;
        let space = PresentSpace::Chat(chat.clone()).into();
        let results = self.keep_results(&space, results, attacher).await?;
        let completion = Completion {
            head,
            cleanup: cleanup || settings.cleanup.done.enabled,
            integrated: false,
            results,
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
        push_closed_and_results(&mut notice, &chat.name, &completion, &closed);
        let about = About::workspace(&chat.name, &chat.id);
        (self.notify_about(about, NotificationKind::WorkspaceDone, notice)).await;
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

    /// Remove `listed` for its completion, under its PR gate, which a
    /// completion changes under.
    async fn sweep_completed_chat(&self, listed: &Chat) -> Result<()> {
        let _guard = self.gates.lock_prs([&listed.id]).await;
        let Some(chat) = self.recorded_chat(&listed.id).await? else {
            return Ok(());
        };
        let Some(completion) = chat.completion.clone() else {
            return Ok(());
        };
        if !completion.cleanup || self.chat_keep_reason(&chat).await?.is_some() {
            return Ok(());
        }
        let removal = self.remove_completed_chat(&chat, &completion).await;
        if !matches!(removal, Ok(false)) {
            notify_chat_removal(self, &chat, "completion cleanup", removal).await;
        }
        Ok(())
    }

    /// Withdraw the completion the chat `selector` names recorded.
    pub async fn undo_chat_done(&self, selector: &str) -> Result<Completion> {
        // A stopped chat withdraws it too, so completion cleanup leaves it.
        let chat =
            (self.recorded_chat(selector).await?).with_context(|| format!("no chat {selector}"))?;
        let _guard = self.gates.lock_prs([&chat.id]).await;
        let id = chat.id.clone();
        self.store.run(move |db| take_completion(db, &id)).await
    }

    async fn done_chat(&self, selector: &str) -> Result<Chat> {
        (self.running_chat(selector).await?).with_context(|| format!("no running chat {selector}"))
    }
}

/// Record `completion` for the running chat `id` and close its open
/// questions, which its finished work waits on no longer; the questions it
/// closed. A completion that would change nothing fails.
fn record_completion(db: &mut Connection, id: &str, completion: &Completion) -> Result<Vec<i64>> {
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
