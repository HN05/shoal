//! A chat's `shoal done`: it records the chat's work as finished, while its
//! agent and session keep running until completion cleanup, when requested,
//! removes the chat as it removes any space.
use crate::client::space_ids::SpaceId;
use anyhow::{Context as _, Result, ensure};

use crate::{
    client::assignments::Done,
    daemon::{
        notifications::{About, NotificationKind},
        progress, questions,
        workspace::{
            Manager,
            done::{self, push_completion_details},
            turn_retry,
        },
        workspace_files::Attacher,
    },
    model::{Chat, Completion, PresentSpace},
};

impl Manager {
    /// Record `done` for the chat `selector` names, at its checkout's HEAD,
    /// with copies of the files it hands back attached by `attacher`; its
    /// open questions close, as a finished workspace's do.
    pub async fn mark_chat_done(
        &self,
        selector: &SpaceId,
        done: Done,
        attacher: &Attacher,
    ) -> Result<Completion> {
        done.check()?;
        ensure!(
            !done.integrated && !done.keep_demos,
            "a chat has no branch or demos; integrated and keep_demos apply to workspaces"
        );
        let chat =
            (self.chat_by_id(selector).await?).with_context(|| format!("no chat {selector}"))?;
        let _guard = self.gates.lock_prs([&chat.id]).await;
        let settings = self.chat_settings(&chat).await?;
        let head = self.chat_head(&chat).await?;
        let space = PresentSpace::Chat(chat.clone()).into();
        let results = self.keep_results(&space, done.results, attacher).await?;
        let completion = Completion {
            head: head.into(),
            cleanup: done.cleanup || settings.cleanup.done.enabled,
            integrated: false,
            results,
            summary: done.summary,
            error: None,
        };
        let (id, recorded) = (chat.id.clone(), completion.clone());
        let now = crate::time::UnixTime::now();
        let (closed, progressed) = (self.store)
            .write(move |tx| {
                // Completion removes what blocked the space.
                let progressed =
                    progress::record_status(tx, &id, recorded.summary.as_deref(), now)?
                        | progress::remove_blockers(tx, &id, now)?;
                Ok((
                    record_completion(tx, &id, &recorded, progressed)?,
                    progressed,
                ))
            })
            .await?;
        if progressed {
            self.publications.request(&chat.id);
        }
        if !closed.is_empty() {
            self.signals.question_changed();
        }
        let mut notice = String::from(match completion.cleanup {
            true => "chat finished; cleanup requested",
            false => "chat finished; no cleanup requested",
        });
        push_completion_details(&mut notice, &chat.number.to_string(), &completion, &closed);
        let about = About::space(&chat.name, &chat.id);
        (self.notify_about(about, NotificationKind::SpaceDone, notice)).await;
        self.signals.wake_cleanup();
        Ok(completion)
    }

    /// Withdraw the completion the chat `id` recorded.
    pub async fn undo_chat_done(&self, id: &SpaceId) -> Result<Completion> {
        // A stopped chat withdraws it too, so completion cleanup leaves it.
        let chat = (self.chat_by_id(id).await?).with_context(|| format!("no chat {id}"))?;
        let _guard = self.gates.lock_prs([&chat.id]).await;
        let id = chat.id.clone();
        (self.store)
            .write(move |tx| done::take_completion(tx, &id)?.context("the chat is not marked done"))
            .await
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
        Ok(head.into())
    }
}

/// Record `completion` for the chat `id` and close its open questions,
/// which its finished work waits on no longer; the questions it closed. A
/// completion that would change nothing, without the status line it
/// `progressed`, fails.
fn record_completion(
    tx: &rusqlite::Transaction<'_>,
    id: &SpaceId,
    completion: &Completion,
    progressed: bool,
) -> Result<Vec<crate::daemon::item_numbers::ItemNumber>> {
    let changed = done::record_completion(tx, id, completion)?;
    let closed = questions::close_open(tx, id)?;
    // Finished work leaves its failed turn alone.
    turn_retry::cancel(tx, id)?;
    ensure!(
        changed || !closed.is_empty() || progressed,
        "the chat is already marked done at this HEAD"
    );
    Ok(closed)
}

#[cfg(test)]
mod tests;
