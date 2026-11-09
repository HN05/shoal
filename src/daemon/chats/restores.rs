//! The daemon's restores of the chats saved to come back on their own: each
//! restores as `shoal resume` restores it, from where it ran, once the agent
//! queue starts agents again.
use std::sync::Arc;

use anyhow::Result;
use rusqlite::params;
use tokio::task::JoinHandle;

use super::{recorded, saved::still_runs};
use crate::{
    daemon::{
        log,
        notifications::{About, NotificationKind},
        workspace::Manager,
    },
    model::Chat,
    state::ChatRestore,
};

/// Restore each chat saved to come back on its own as `shoal resume`
/// restores it, once the agent queue starts agents again, so not before a
/// shutdown under way. Each restore is claimed first, so a chat whose
/// restore fails waits for `shoal resume`, and the user learns why.
/// Returns the restores, which run on their own.
pub(super) async fn restore_automatic(manager: &Arc<Manager>) -> Result<Vec<JoinHandle<()>>> {
    // A shutdown holds the queue before it saves chats, so a claim after
    // its save, which would find their agents running, sees the hold.
    let saving = manager.chat_saves.lock().await;
    if manager.agent_queue.suspended() {
        return Ok(Vec::new());
    }
    let owner = manager.root_dir_owner();
    let claimed = (manager.store)
        .write(move |tx| claim_automatic(tx, &owner))
        .await?;
    drop(saving);
    Ok(claimed
        .into_iter()
        .map(|chat| {
            let manager = manager.clone();
            tokio::spawn(async move {
                match resume(&manager.paths, &chat, None).await {
                    Ok(Ok(())) => {}
                    // Its own failures reach the user from it.
                    Ok(Err(error)) => log!("chat {}: shoal resume failed: {error}", chat.id),
                    Err(error) => {
                        let error = format!("{error:#}");
                        manager.notify_restore_failed(&chat, &error).await;
                    }
                }
            })
        })
        .collect())
}

impl Manager {
    /// Tell the user that `chat` waits for `shoal resume` since its restore
    /// failed with `error`.
    pub(super) async fn notify_restore_failed(&self, chat: &Chat, error: &str) {
        log!("chat {} not restored: {error}", chat.id);
        self.notify_about(
            About::space(&chat.name, &chat.id),
            NotificationKind::ChatRestoreFailed,
            format!(
                "Could not restore chat {}: {error}; restore it with shoal resume {}",
                chat.name, chat.name
            ),
        )
        .await;
    }
}

/// Make the chats saved to come back on their own wait for `shoal resume`,
/// returning them, unless one still runs, which runs on.
fn claim_automatic(tx: &rusqlite::Transaction<'_>, owner: &std::path::Path) -> Result<Vec<Chat>> {
    let saved: Vec<Chat> = recorded(tx, owner)?
        .into_iter()
        .filter(|chat| chat.restore == Some(ChatRestore::Automatic))
        .collect();
    let mut claimed = Vec::new();
    for chat in saved {
        let restore = match still_runs(tx, &chat)? {
            true => None,
            false => Some(ChatRestore::Resume),
        };
        tx.execute(
            "UPDATE chats SET restore=?2,
                stopped_at=CASE WHEN ?2 IS NULL THEN NULL ELSE stopped_at END,
                saved=CASE WHEN ?2 IS NULL THEN 0 ELSE saved END
             WHERE id=?1",
            params![chat.id, restore],
        )?;
        if restore.is_some() {
            claimed.push(chat);
        }
    }
    Ok(claimed)
}

/// Run `shoal resume` for `chat` detached from the daemon, as it would run
/// from where the chat ran, under `narrowing` when given, and wait for it to
/// open the chat's place. Returns how it ended, with the error output of one
/// that failed; an error is one to run it.
#[cfg(not(test))]
pub(super) async fn resume(
    paths: &crate::paths::Paths,
    chat: &Chat,
    narrowing: Option<crate::config::permissions::PermissionsConfig>,
) -> Result<Result<(), String>> {
    use anyhow::Context as _;
    use std::process::Stdio;

    let mut argv = crate::workers::shoal_argv(paths)?;
    argv.extend(["resume".into(), chat.id.clone().into()]);
    for assignment in narrowing
        .iter()
        .flat_map(|narrowing| narrowing.assignments())
    {
        argv.extend(["--permission".into(), assignment.into()]);
    }
    let env = crate::terminal::detached_start_environment(chat.origin.as_ref());
    let mut command = tokio::process::Command::new(&argv[0]);
    command
        .args(&argv[1..])
        .envs(&env)
        .current_dir(&paths.home)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped());
    // SAFETY: setsid detaches the forked child without allocating, before exec.
    unsafe {
        command.pre_exec(|| {
            if libc::setsid() == -1 {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        });
    }
    let output = command.output().await.context("run shoal resume")?;
    Ok(match output.status.success() {
        true => Ok(()),
        false => Err(String::from_utf8_lossy(&output.stderr).trim().to_owned()),
    })
}

/// Tests record the chats restored instead of running this binary.
#[cfg(test)]
pub(super) async fn resume(
    _paths: &crate::paths::Paths,
    chat: &Chat,
    _narrowing: Option<crate::config::permissions::PermissionsConfig>,
) -> Result<Result<(), String>> {
    tests::RESUMED.lock().unwrap().push(chat.id.clone());
    anyhow::ensure!(
        !tests::UNRESUMABLE
            .lock()
            .unwrap()
            .contains(&chat.name.to_string()),
        "no terminal host"
    );
    while tests::HELD.lock().unwrap().contains(&chat.name.to_string()) {
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
    Ok(Ok(()))
}

#[cfg(test)]
pub(super) mod tests;
