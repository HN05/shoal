//! Chats: agents the user talks with in a repository's registered checkout.
//! They are records, not tracked executions, so they never queue, stop under
//! pressure or resume; a chat lasts while its process lives.
use std::{
    path::{Path, PathBuf},
    sync::Arc,
    time::Duration,
};

use anyhow::{Context as _, Result, ensure};
use rusqlite::{Connection, OptionalExtension, params};
use uuid::Uuid;

use crate::{
    daemon::{
        chat_messages, log, store,
        workspace::{Manager, derive_workspace_name},
    },
    model::{Chat, OpenedWorkspace, Repository, Workspace},
    process::identity::{self, Identity},
    tmp_dirs,
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
        // Its temporary directory goes in the repository's directory.
        let repository = self.reserve_repository_directory(&repository).await?;
        let process = identity::capture(pid)?.context("the chat process is not running")?;
        let started_at = i64::try_from(crate::time::unix_seconds())?;
        self.end_finished_chats().await?;
        // A place Shoal created for a workspace is the chat's now.
        if let Some(origin) = &origin {
            self.release_place(origin).await?;
        }
        let chat = Chat {
            id: Uuid::new_v4().to_string(),
            repository_id: repository.id.clone(),
            name,
            agent,
            pid: process.pid,
            started_at,
            origin,
            tmp_dir: None,
        };
        let birth = process.birth.clone();
        let chat = self
            .store
            .run(move |db| insert(db, chat, &birth, hands_over))
            .await?;
        self.give_tmp_dir(chat, &repository, process).await
    }

    /// `chat`, just recorded with `process`, with its temporary directory,
    /// created now; a chat that cannot have one ends.
    async fn give_tmp_dir(
        &self,
        mut chat: Chat,
        repository: &Repository,
        process: Identity,
    ) -> Result<Chat> {
        match tmp_dirs::directory(repository, &chat.id) {
            Ok(tmp_dir) => {
                chat.tmp_dir = tmp_dir;
                Ok(chat)
            }
            Err(error) => {
                self.end_chats(vec![(chat.id, process)]).await?;
                Err(error)
            }
        }
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

    /// Whether the chat `id` is still recorded, without first ending chats
    /// whose process exited.
    pub(crate) async fn chat_recorded(&self, id: &str) -> Result<bool> {
        let id = id.to_owned();
        self.store
            .run(move |db| store::exists(db, "SELECT 1 FROM chats WHERE id=?1", [&id]))
            .await
    }

    /// The running chat with the ID or name `selector`, if any.
    pub(crate) async fn running_chat(&self, selector: &str) -> Result<Option<Chat>> {
        let chats = self.chats().await?;
        Ok(chats
            .into_iter()
            .find(|chat| chat.id == selector || chat.name == selector))
    }

    /// End chats whose process has exited.
    pub(crate) async fn end_finished_chats(&self) -> Result<()> {
        let ended = finished(self.store.run(recorded_processes).await?);
        self.end_chats(ended).await
    }

    /// End the chats still recorded with the process found ended: return
    /// their workspaces to the user along with the messages the chats had not
    /// received, delete their records and temporary directories, and keep
    /// their attached files for as long as their repository says. A chat
    /// moved to another process since keeps running.
    async fn end_chats(&self, ended: Vec<(String, Identity)>) -> Result<()> {
        if ended.is_empty() {
            return Ok(());
        }
        let deleted = self.store.run(move |db| delete(db, &ended)).await?;
        for (repository_id, id) in deleted.chats {
            if let Err(error) = self.delete_tmp_dir(&repository_id, &id).await {
                log!("chat {id}: temporary directory not deleted: {error:#}");
            }
            // The cleanup sweep stamps what this misses.
            if let Err(error) = self.stamp_ended_chat_files(&repository_id).await {
                log!("chat {id}: attached files not given an expiry: {error:#}");
            }
        }
        if let Some(id) = deleted.notification {
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
        crate::daemon::workspace_files::rename_chat(&tx, &old.id, name)?;
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
            "SELECT chats.id,chats.repository_id,chats.name,chats.agent,chats.pid,
                    chats.started_at,chats.origin,repositories.workspaces_dir
             FROM chats JOIN repositories ON repositories.id=chats.repository_id
             ORDER BY chats.started_at, chats.rowid",
        )?
        .query_map([], |row| {
            let id: String = row.get(0)?;
            let repository_id: String = row.get(1)?;
            let tmp_dir = row.get::<_, Option<String>>(7)?.map(|directory| {
                tmp_dirs::Layout::new(Path::new(&directory), &repository_id).locate(&id)
            });
            Ok(Chat {
                id,
                repository_id,
                name: row.get(2)?,
                agent: row.get(3)?,
                pid: row.get(4)?,
                started_at: row.get(5)?,
                origin: store::json_column(row, "origin")?,
                tmp_dir,
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

/// The chat records [`delete`] deleted.
struct Deleted {
    /// The last notification recorded.
    notification: Option<i64>,
    /// The repository and ID of each chat.
    chats: Vec<(String, String)>,
}

/// Delete the records of the chats still recorded with the process found
/// ended, giving the user their unread messages.
fn delete(db: &mut Connection, ended: &[(String, Identity)]) -> Result<Deleted> {
    let tx = db.transaction()?;
    let mut last = None;
    let mut deleted = Vec::new();
    for (id, process) in ended {
        let repository_id: Option<String> = tx
            .query_row(
                "SELECT repository_id FROM chats WHERE id=?1 AND pid=?2 AND birth=?3",
                params![id, process.pid, process.birth],
                |row| row.get(0),
            )
            .optional()?;
        let Some(repository_id) = repository_id else {
            continue;
        };
        last = chat_messages::hand_to_user(&tx, id)?.or(last);
        tx.execute("DELETE FROM chats WHERE id=?1", [id])?;
        deleted.push((repository_id, id.clone()));
    }
    tx.commit()?;
    Ok(Deleted {
        notification: last,
        chats: deleted,
    })
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
