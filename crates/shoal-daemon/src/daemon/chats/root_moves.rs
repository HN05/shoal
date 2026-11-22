//! The files of the chats without a repository, which `shoal repo move`
//! moves into the directory that holds the repositories once it is done:
//! the temporary directory, attached files and kept session files of each
//! such chat, running, stopped or removed, whose records then name the configured root, which
//! is that directory or links to it.
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, ensure};
use rusqlite::params;

use crate::{daemon::manager::Manager, fsutil, tmp_dirs};
use shoal_ids::space_ids::SpaceId;
use shoal_protocol::space_names::SpaceName;
use shoal_repositories::moves::{Running, chats::OutsideChat};

impl Manager {
    /// The directories in `root` that keep the temporary directories,
    /// attached files and kept session files of the chats without a
    /// repository, which move with the chats, and the packages imported
    /// for them, where they are this Shoal's; all leave the root.
    pub(crate) fn outside_dirs(&self, root: &Path) -> Vec<PathBuf> {
        let owner = self.root_dir_owner();
        let tmp = tmp_dirs::Layout::outside_repositories(root, &owner);
        let files = self.outside_files_layout(root);
        let sessions = self.outside_sessions_layout(root);
        let imports = self.outside_imports(root);
        (tmp.own_root().into_iter())
            .chain(files.own_root())
            .chain(sessions.own_root())
            .chain(imports.is_own().then(|| imports.root()))
            .map(Path::to_owned)
            .collect()
    }

    /// The chats without a repository, running, stopped or removed, whose
    /// files or records a move to `root_dir`, which names or links to
    /// `destination`, changes, refusing one that runs unless `running` has
    /// the caller stop it first.
    pub(crate) async fn outside_chats_moving(
        &self,
        root_dir: &Path,
        destination: &Path,
        running: Running,
    ) -> Result<Vec<OutsideChat>> {
        let mut moving = Vec::new();
        for chat in self.outside_chats().await? {
            if !moves(&chat, root_dir, destination) {
                continue;
            }
            ensure!(
                running == Running::StoppedFirst || chat.running.is_empty(),
                "chat {} is running in root_dir; stop it first",
                chat.name
            );
            moving.push(chat);
        }
        self.ensure_no_import_left(&moving, root_dir, destination)
            .await?;
        Ok(moving)
    }

    /// Refuse while an import reads a package into a root a move of
    /// `chats` to `root_dir`, which names or links to `destination`,
    /// leaves.
    async fn ensure_no_import_left(
        &self,
        chats: &[OutsideChat],
        root_dir: &Path,
        destination: &Path,
    ) -> Result<()> {
        for root in roots_left(chats, root_dir, destination) {
            self.ensure_no_import_in(self.outside_imports(&root))
                .await?;
        }
        Ok(())
    }

    /// Every chat without a repository, running, stopped or removed.
    async fn outside_chats(&self) -> Result<Vec<OutsideChat>> {
        let configured = self.config().root_dir(&self.paths)?;
        let owner = self.root_dir_owner();
        let mut chats = Vec::new();
        for (id, name, root_dir) in self.outside_homes().await? {
            let root_dir = root_dir.unwrap_or_else(|| configured.clone());
            let tmp = tmp_dirs::Layout::outside_repositories(&root_dir, &owner).existing(&id);
            let files = self.outside_files_layout(&root_dir).existing(&id);
            let sessions = self.outside_sessions_layout(&root_dir).existing(&id);
            let resolved = |path: Option<PathBuf>| {
                (path.map(std::fs::canonicalize).transpose())
                    .with_context(|| format!("find the files of chat {name}"))
            };
            chats.push(OutsideChat {
                tmp: resolved(tmp)?,
                files: resolved(files)?,
                sessions: resolved(sessions)?,
                running: self.workspace_executions(&id).await?,
                root_dir,
                id,
                name,
            });
        }
        Ok(chats)
    }

    /// The ID, name and recorded `root_dir` of each chat without a
    /// repository, running or stopped, or removed with a removal record or
    /// attached files.
    async fn outside_homes(&self) -> Result<Vec<(SpaceId, SpaceName, Option<PathBuf>)>> {
        let mut homes: Vec<(SpaceId, SpaceName, Option<PathBuf>)> = Vec::new();
        let live = (self.chats().await?.into_iter())
            .filter(|chat| chat.repository_id.is_none())
            .map(|chat| (chat.id, chat.name, chat.workdir.root_dir().map(Into::into)));
        let removed = (self.removed_spaces().await?.into_iter())
            .filter(|removed| removed.repository_id.is_none())
            .filter_map(|removed| {
                let root_dir = removed.chat()?.root_dir.clone();
                Some((removed.space_id, removed.name, root_dir))
            });
        let files = self.store.run(|db| outside_file_homes(db)).await?;
        for (id, name, root_dir) in live.chain(removed).chain(files) {
            if !homes.iter().any(|(known, ..)| *known == id) {
                homes.push((id, name, root_dir));
            }
        }
        Ok(homes)
    }

    /// Move the files of each of `chats` into `destination`, the directory
    /// the root `root_dir` names or links to once the move is done, and
    /// record that each runs there from now on, then delete what is left in
    /// the roots they left, `root_dir` itself among them while it is not
    /// `destination` yet. Refused while an import reads a package into one
    /// of those roots; the caller holds the gate of those chats' files.
    pub(crate) async fn move_outside_chats(
        &self,
        chats: Vec<OutsideChat>,
        root_dir: &Path,
        destination: &Path,
    ) -> Result<()> {
        let left = roots_left(&chats, root_dir, destination);
        self.ensure_no_import_left(&chats, root_dir, destination)
            .await?;
        self.move_chat_files(chats, root_dir, destination).await?;
        for root in left {
            let tmp = tmp_dirs::Layout::outside_repositories(&root, &self.root_dir_owner());
            let files = self.outside_files_layout(&root);
            let sessions = self.outside_sessions_layout(&root);
            let imports = self.outside_imports(&root);
            let removed = move || {
                (tmp.remove())
                    .and(files.remove())
                    .and(sessions.remove())
                    .and(imports.remove())
            };
            tokio::task::spawn_blocking(removed).await??;
        }
        Ok(())
    }

    /// Move the files of `chats` into `destination` and record that each
    /// runs in `root_dir`, as [`Self::move_outside_chats`] does.
    async fn move_chat_files(
        &self,
        chats: Vec<OutsideChat>,
        root_dir: &Path,
        destination: &Path,
    ) -> Result<()> {
        let owner = self.root_dir_owner();
        let tmp = tmp_dirs::Layout::outside_repositories(destination, &owner);
        let files = self.outside_files_layout(destination);
        let sessions = self.outside_sessions_layout(destination);
        for chat in chats {
            let name = &chat.name;
            if let Some(source) = elsewhere(&chat.tmp, destination) {
                let moved = move_into(source, tmp.prepare(&chat.id)?);
                moved
                    .await
                    .with_context(|| format!("move the files of chat {name}"))?;
            }
            if let Some(source) = elsewhere(&chat.files, destination) {
                let moved = move_into(source, files.prepare(&chat.id)?);
                moved
                    .await
                    .with_context(|| format!("move the files of chat {name}"))?;
            }
            if let Some(source) = elsewhere(&chat.sessions, destination) {
                let moved = move_into(source, sessions.prepare(&chat.id)?);
                moved
                    .await
                    .with_context(|| format!("move the session files of chat {name}"))?;
            }
            let (id, root_dir) = (chat.id.clone(), root_dir.to_owned());
            (self.store)
                .write(move |tx| rehome(tx, &id, &root_dir))
                .await?;
        }
        Ok(())
    }
}

/// Whether a move to `root_dir`, which names or links to `destination`,
/// changes where `chat` runs or keeps its files.
fn moves(chat: &OutsideChat, root_dir: &Path, destination: &Path) -> bool {
    chat.root_dir != root_dir
        || elsewhere(&chat.tmp, destination).is_some()
        || elsewhere(&chat.files, destination).is_some()
        || elsewhere(&chat.sessions, destination).is_some()
}

/// The `root_dir` the files of `chat` are in now, resolved.
fn source_root(chat: &OutsideChat) -> Option<PathBuf> {
    let directory = (chat.tmp.as_ref())
        .or(chat.files.as_ref())
        .or(chat.sessions.as_ref())?;
    Some(directory.parent()?.parent()?.to_owned())
}

/// The roots a move of `chats` to `root_dir`, which names or links to
/// `destination`, leaves: those their files are in, and `root_dir` itself
/// while it is not `destination` yet.
fn roots_left(chats: &[OutsideChat], root_dir: &Path, destination: &Path) -> Vec<PathBuf> {
    let mut left: Vec<PathBuf> = (chats.iter())
        .filter_map(source_root)
        .chain(std::fs::canonicalize(root_dir))
        .filter(|left| left != destination)
        .collect();
    left.sort();
    left.dedup();
    left
}

/// `directory`, when it is not in `destination`.
fn elsewhere<'a>(directory: &'a Option<PathBuf>, destination: &Path) -> Option<&'a Path> {
    (directory.as_deref()).filter(|directory| !directory.starts_with(destination))
}

/// Move the directory `source` onto the new, empty one `destination`.
async fn move_into(source: &Path, destination: PathBuf) -> Result<()> {
    if fsutil::copy::move_directory(source, &destination).await? == fsutil::copy::Moved::Copied {
        crate::shoal_dirs::remove_dir(source)?;
    }
    Ok(())
}

/// The ID, name and `root_dir` of each removed chat without a repository
/// whose attached files are kept.
fn outside_file_homes(
    db: &rusqlite::Connection,
) -> Result<Vec<(SpaceId, SpaceName, Option<PathBuf>)>> {
    Ok(db
        .prepare(
            "SELECT DISTINCT space_id,space,root_dir FROM space_files
                WHERE repository_id IS NULL ORDER BY space_id",
        )?
        .query_map([], |row| {
            let root_dir: Option<String> = row.get(2)?;
            Ok((row.get(0)?, row.get(1)?, root_dir.map(PathBuf::from)))
        })?
        .collect::<rusqlite::Result<_>>()?)
}

/// Record that the chat without a repository `id` runs in `root_dir`, with
/// its attached files there: its record, its removal record and the paths
/// of its attached files, which named the root it ran in before.
fn rehome(tx: &rusqlite::Transaction<'_>, id: &SpaceId, root_dir: &Path) -> Result<()> {
    tx.execute(
        "UPDATE chats SET root_dir=?2 WHERE id=?1",
        params![id, root_dir.to_string_lossy()],
    )?;
    crate::daemon::restore::rehome_removed_chat(tx, id, root_dir)?;
    let recorded: Vec<(i64, Option<String>, String)> = tx
        .prepare(
            "SELECT id,root_dir,path FROM space_files
                WHERE repository_id IS NULL AND space_id=?1",
        )?
        .query_map([id], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)))?
        .collect::<rusqlite::Result<_>>()?;
    for (record, old, path) in recorded {
        let path = match old.as_deref().map(|old| Path::new(&path).strip_prefix(old)) {
            Some(Ok(inside)) => root_dir.join(inside),
            _ => PathBuf::from(path),
        };
        tx.execute(
            "UPDATE space_files SET root_dir=?2, path=?3 WHERE id=?1",
            params![record, root_dir.to_string_lossy(), path.to_string_lossy()],
        )?;
    }
    Ok(())
}
