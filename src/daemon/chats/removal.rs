//! The end of a removed chat's record, once the spaces below it and its
//! agent are gone: the user gets the messages it had not read and the
//! approval requests it had not decided, its attached files stay for the
//! retention period, and its record and temporary directory go.
use anyhow::{Result, ensure};
use rusqlite::Connection;

use crate::{
    daemon::{
        approvals::{ApprovalRequest, release_decided_by},
        chat_messages, log, store,
        workspace::Manager,
        workspace_files,
    },
    model::Chat,
};

impl Manager {
    /// Delete the record of the stopped `chat` with what only it kept.
    pub(crate) async fn delete_chat(&self, chat: &Chat) -> Result<()> {
        let expires_at = self.chat_files_expiry(&chat.repository_id).await;
        let id = chat.id.clone();
        let deleted = self
            .store
            .run(move |db| delete(db, &id, expires_at))
            .await?;
        if let Err(error) = self.delete_tmp_dir(&chat.repository_id, &chat.id).await {
            log!(
                "chat {}: temporary directory not deleted: {error:#}",
                chat.id
            );
        }
        self.ask_user_about(&deleted.released).await;
        if let Some(id) = deleted.notification {
            self.signals.notification_recorded(id);
        }
        Ok(())
    }
}

/// What [`delete`] gave the user.
struct Deleted {
    /// The last notification recorded for the chat's unread messages.
    notification: Option<i64>,
    /// The approval requests the chat had not decided.
    released: Vec<ApprovalRequest>,
}

/// Delete the record of the stopped chat `id`, giving the user its unread
/// messages and keeping its attached files until `expires_at`.
fn delete(db: &mut Connection, id: &str, expires_at: i64) -> Result<Deleted> {
    let tx = db.transaction()?;
    ensure!(
        store::exists(
            &tx,
            "SELECT 1 FROM chats WHERE id=?1 AND restore IS NOT NULL",
            [id]
        )?,
        "the chat runs again"
    );
    // The spaces below go first; none may outlive it.
    ensure!(
        crate::daemon::workspace::children(&tx, id)?.is_empty(),
        "workspaces were opened from it during removal; retaining it"
    );
    let notification = chat_messages::hand_to_user(&tx, id)?;
    workspace_files::keep_ended_chat(&tx, id, expires_at)?;
    // With the record, so no request is left with a removed decider.
    let released = release_decided_by(&tx, id)?;
    tx.execute("DELETE FROM chats WHERE id=?1", [id])?;
    tx.commit()?;
    Ok(Deleted {
        notification,
        released,
    })
}
