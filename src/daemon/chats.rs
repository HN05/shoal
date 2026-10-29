//! Chats: agents the user talks with in a repository's registered checkout.
//! They are records, not tracked executions, so they never queue, stop under
//! pressure or resume; a chat lasts while its process lives.
use std::{path::PathBuf, sync::Arc, time::Duration};

use anyhow::{Context as _, Result, ensure};
use rusqlite::{Connection, params};
use uuid::Uuid;

use crate::{
    daemon::{
        chat_messages, log, store,
        workspace::{Manager, derive_workspace_name},
    },
    model::{Chat, OpenedWorkspace, Workspace},
    process::identity::{self, Identity},
};

/// How soon a chat's end reaches the journal when nothing lists chats.
const SWEEP_INTERVAL: Duration = Duration::from_secs(5);

pub(super) async fn run(manager: Arc<Manager>) {
    loop {
        tokio::time::sleep(SWEEP_INTERVAL).await;
        let _operation = manager.gates.operation().await;
        if let Err(error) = manager.end_finished_chats().await {
            log!("chats: {error:#}");
        }
    }
}

impl Manager {
    /// Record the chat `name` whose agent replaces process `pid` in the
    /// repository's checkout, or when `hands_over`, runs in a session `pid`
    /// starts and [`Self::move_chat`] names.
    pub async fn start_chat(
        &self,
        repository: crate::repository::Selector,
        name: String,
        agent: String,
        pid: u32,
        origin: Option<crate::sessions::Origin>,
        hands_over: bool,
    ) -> Result<Chat> {
        crate::validate::name("chat", &name)?;
        let repository = self.repository(repository).await?.id;
        let process = identity::capture(pid)?.context("the chat process is not running")?;
        let started_at = i64::try_from(crate::time::unix_seconds())?;
        self.end_finished_chats().await?;
        // A place Shoal created for a workspace is the chat's now.
        if let Some(origin) = &origin {
            self.release_place(origin).await?;
        }
        let chat = Chat {
            id: Uuid::new_v4().to_string(),
            repository_id: repository,
            name,
            agent,
            pid: process.pid,
            started_at,
            origin,
        };
        self.store
            .run(move |db| insert(db, chat, &process.birth, hands_over))
            .await
    }

    /// Refuse a name a new chat could not take now.
    pub async fn check_chat_name(&self, name: String) -> Result<()> {
        crate::validate::name("chat", &name)?;
        self.end_finished_chats().await?;
        self.store
            .run(move |db| store::ensure_name_available(db, &name, None))
            .await
    }

    /// `name`, made a valid name, or its lowest `-2`, `-3`, ... variant
    /// that a new chat could take now.
    pub async fn free_chat_name(&self, name: String) -> Result<String> {
        let base = derive_workspace_name(&name);
        self.end_finished_chats().await?;
        self.store.run(move |db| free_name(db, &base)).await
    }

    /// Give the running chat `chat`, by ID or name, the name `name`, which
    /// must be free as a new chat's, and move its files to that name.
    pub async fn rename_chat(&self, chat: String, name: String) -> Result<Chat> {
        crate::validate::name("chat", &name)?;
        self.end_finished_chats().await?;
        let paths = self.paths.clone();
        // The store's one thread moves the files after the rename commits,
        // so no other rename or chat start reaches either name in between.
        self.store
            .run(move |db| {
                let (old, renamed) = rename(db, &chat, &name)?;
                // The chat has its new name even when its files stay behind.
                if old.name != renamed.name
                    && let Err(error) = move_chat_files(&paths, &old, &renamed)
                {
                    log!("chat {}: {error:#}", renamed.id);
                }
                Ok(renamed)
            })
            .await
    }

    /// Make process `pid` the one whose life a running chat lasts.
    pub async fn move_chat(&self, chat: &str, pid: u32) -> Result<Chat> {
        let process = identity::capture(pid)?.context("the chat process is not running")?;
        let chat = chat.to_owned();
        self.store.run(move |db| move_to(db, &chat, &process)).await
    }

    /// Running chats in start order, after ending those whose process exited.
    pub async fn chats(&self) -> Result<Vec<Chat>> {
        self.end_finished_chats().await?;
        self.store.run(|db| running(db)).await
    }

    /// Delete chats whose process has exited, which returns their workspaces
    /// to the user along with the messages the chats had not received.
    pub(crate) async fn end_finished_chats(&self) -> Result<()> {
        let ended = finished(self.store.run(recorded_processes).await?);
        if ended.is_empty() {
            return Ok(());
        }
        if let Some(id) = self.store.run(move |db| delete(db, &ended)).await? {
            self.signals.notification_recorded(id);
        }
        Ok(())
    }

    /// Give `workspace` to the running chat that opened it, or to the user
    /// when it was opened outside one or the chat has ended.
    pub async fn assign_chat(&self, workspace: &str, chat: Option<String>) -> Result<()> {
        let workspace = workspace.to_owned();
        self.store
            .run(move |db| assign(db, &workspace, chat.as_deref()))
            .await
    }

    /// [`Self::open_branch`] for `chat`, returning the workspace with its chat.
    pub async fn open_branch_for_chat(
        &self,
        repository: crate::repository::Selector,
        branch: &str,
        git_profile: Option<&str>,
        path: Option<PathBuf>,
        base: Option<String>,
        chat: Option<String>,
    ) -> Result<OpenedWorkspace> {
        let mut opened = self
            .open_branch(repository, branch, git_profile, path, base)
            .await?;
        opened.workspace = self.give_to_chat(&opened.workspace.id, chat).await?;
        Ok(opened)
    }

    /// [`Self::create_workspace`] for `chat`, returning the workspace with its
    /// chat.
    pub async fn create_workspace_for_chat(
        &self,
        repository: crate::repository::Selector,
        name: String,
        base: Option<String>,
        git_profile: Option<&str>,
        path: Option<PathBuf>,
        chat: Option<String>,
    ) -> Result<Workspace> {
        let workspace = self
            .create_workspace(repository, name, base, git_profile, path)
            .await?;
        self.give_to_chat(&workspace.id, chat).await
    }

    /// Assign `workspace` to `chat` and read it again.
    async fn give_to_chat(&self, workspace: &str, chat: Option<String>) -> Result<Workspace> {
        self.assign_chat(workspace, chat).await?;
        self.workspace(workspace).await
    }
}

/// Insert `chat` unless its name is taken or the repository is being removed,
/// starting until it is moved when it `hands_over`.
fn insert(db: &mut Connection, chat: Chat, birth: &str, hands_over: bool) -> Result<Chat> {
    let tx = db.transaction()?;
    anyhow::ensure!(
        !store::repository_removing(&tx, &chat.repository_id)?,
        "the repository is being removed"
    );
    // A chat edits the checkout, which a move may be copying.
    store::ensure_repository_not_moving(&tx, &chat.repository_id)?;
    store::ensure_name_available(&tx, &chat.name, None)?;
    tx.execute(
        "INSERT INTO chats(id,repository_id,name,agent,pid,birth,started_at,origin,starting)
         VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9)",
        params![
            chat.id,
            chat.repository_id,
            chat.name,
            chat.agent,
            chat.pid,
            birth,
            chat.started_at,
            chat.origin
                .as_ref()
                .map(serde_json::to_string)
                .transpose()?,
            hands_over
        ],
    )?;
    tx.commit()?;
    Ok(chat)
}

/// The first of `base`, `base-2`, `base-3`, ... that nothing holds, each cut
/// to the longest name allowed.
fn free_name(db: &mut Connection, base: &str) -> Result<String> {
    for suffix in 1_u64.. {
        let candidate = suffixed(base, suffix);
        if !store::name_taken(db, &candidate)? {
            return Ok(candidate);
        }
    }
    unreachable!("an unused suffix exists")
}

/// `base`, or `base-<suffix>` from 2 on, shortened to fit the name limit.
pub(crate) fn suffixed(base: &str, suffix: u64) -> String {
    let suffix = match suffix {
        1 => String::new(),
        n => format!("-{n}"),
    };
    let kept = crate::validate::MAX_NAME_LEN.saturating_sub(suffix.len());
    let mut name: String = base.chars().take(kept).collect();
    name.push_str(&suffix);
    name
}

/// Rename the running chat `chat`, by ID or name, to `name` unless another
/// holds it. Returns the chat before and after.
fn rename(db: &mut Connection, chat: &str, name: &str) -> Result<(Chat, Chat)> {
    let tx = db.transaction()?;
    let old = running(&tx)?
        .into_iter()
        .find(|running| running.id == chat || running.name == chat)
        .with_context(|| format!("no running chat {chat}"))?;
    if old.name != name {
        // The starting chat's files are not open yet under its name.
        ensure!(
            !store::exists(
                &tx,
                "SELECT 1 FROM chats WHERE id=?1 AND starting",
                [&old.id]
            )?,
            "chat {} is still starting; rename it once it runs",
            old.name
        );
        store::ensure_name_available(&tx, name, None)?;
        tx.execute(
            "UPDATE chats SET name=?2 WHERE id=?1",
            params![old.id, name],
        )?;
    }
    tx.commit()?;
    let renamed = Chat {
        name: name.to_owned(),
        ..old.clone()
    };
    Ok((old, renamed))
}

/// Move the files kept under `old`'s name to `renamed`'s, replacing those
/// an ended chat of that name left. A running session keeps writing to the
/// log it holds open.
fn move_chat_files(paths: &crate::paths::Paths, old: &Chat, renamed: &Chat) -> Result<()> {
    let (from, to) = (paths.chat_files(old), paths.chat_files(renamed));
    for extension in [crate::paths::SESSION_LOG, crate::paths::SESSION_PROMPT] {
        let (from, to) = (from.with_extension(extension), to.with_extension(extension));
        match std::fs::rename(&from, &to) {
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            moved => moved.with_context(|| format!("cannot move {}", from.display()))?,
        }
    }
    Ok(())
}

fn move_to(db: &mut Connection, chat: &str, process: &Identity) -> Result<Chat> {
    let moved = db.execute(
        "UPDATE chats SET pid=?2,birth=?3,starting=0 WHERE id=?1",
        params![chat, process.pid, process.birth],
    )?;
    anyhow::ensure!(moved == 1, "unknown chat {chat}");
    running(db)?
        .into_iter()
        .find(|running| running.id == chat)
        .context("the chat ended")
}

fn running(db: &Connection) -> Result<Vec<Chat>> {
    Ok(db
        .prepare(
            "SELECT id,repository_id,name,agent,pid,started_at,origin FROM chats
             ORDER BY started_at, rowid",
        )?
        .query_map([], |row| {
            Ok(Chat {
                id: row.get(0)?,
                repository_id: row.get(1)?,
                name: row.get(2)?,
                agent: row.get(3)?,
                pid: row.get(4)?,
                started_at: row.get(5)?,
                origin: store::json_column(row, "origin")?,
            })
        })?
        .collect::<rusqlite::Result<_>>()?)
}

fn recorded_processes(db: &mut Connection) -> Result<Vec<(String, Identity)>> {
    Ok(db
        .prepare("SELECT id,pid,birth FROM chats")?
        .query_map([], |row| {
            Ok((
                row.get(0)?,
                Identity {
                    pid: row.get(1)?,
                    birth: row.get(2)?,
                    unique: None,
                },
            ))
        })?
        .collect::<rusqlite::Result<_>>()?)
}

/// The chats whose process no longer has its recorded identity. A process
/// that cannot be read keeps its chat.
fn finished(recorded: Vec<(String, Identity)>) -> Vec<(String, Identity)> {
    recorded
        .into_iter()
        .filter(|(id, process)| match identity::capture(process.pid) {
            Ok(current) => current.as_ref() != Some(process),
            Err(error) => {
                log!("chat {id}: {error:#}");
                false
            }
        })
        .collect()
}

/// Delete the chats still recorded with the process found ended, giving the
/// user their unread messages; a chat moved to another process since keeps
/// running. Returns the last notification recorded.
fn delete(db: &mut Connection, ended: &[(String, Identity)]) -> Result<Option<i64>> {
    let tx = db.transaction()?;
    let mut last = None;
    for (id, process) in ended {
        let unchanged: bool = tx.query_row(
            "SELECT EXISTS(SELECT 1 FROM chats WHERE id=?1 AND pid=?2 AND birth=?3)",
            params![id, process.pid, process.birth],
            |row| row.get(0),
        )?;
        if !unchanged {
            continue;
        }
        last = chat_messages::hand_to_user(&tx, id)?.or(last);
        tx.execute("DELETE FROM chats WHERE id=?1", [id])?;
    }
    tx.commit()?;
    Ok(last)
}

/// An unknown chat leaves the workspace to the user.
fn assign(db: &mut Connection, workspace: &str, chat: Option<&str>) -> Result<()> {
    db.execute(
        "UPDATE workspaces SET chat_id=(SELECT id FROM chats WHERE id=?2) WHERE id=?1",
        params![workspace, chat],
    )?;
    Ok(())
}

#[cfg(test)]
mod tests;
