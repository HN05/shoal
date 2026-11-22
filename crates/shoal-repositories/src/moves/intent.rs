//! The recorded intent of a repository directory move: recorded before the
//! directory moves, switched once the records name its destination, and
//! finished or rolled back from what is on disk.
use std::{
    collections::BTreeMap,
    fs,
    path::{Path, PathBuf},
};

use anyhow::{Context, Result, ensure};
use rusqlite::{Connection, OptionalExtension, params};
use shoal_core::{fsutil::off_runtime, git};
use shoal_daemon_base::{
    log,
    ownership::{
        directory_identity, has_directory_identity,
        paths::{is_real_directory, real_directory_identity},
    },
};
use shoal_ids::{repository_ids::RepositoryId, space_ids::SpaceId};
use shoal_protocol::{
    model::{Repository, Workspace},
    state::WorkspaceState,
};
use shoal_store as store;

use super::Running;
use crate::{Repositories, daemon::Daemon, daemon::Records};

/// A recorded repository directory move.
struct Intent {
    repository_id: RepositoryId,
    source: PathBuf,
    destination: PathBuf,
    /// The source directory's identity, which a rename carries to the
    /// destination.
    source_id: String,
    /// The identity of the reserved, empty destination.
    destination_id: String,
    /// The state each workspace of the repository returns to.
    states: BTreeMap<SpaceId, WorkspaceState>,
    /// The records name the destination.
    switched: bool,
}

impl Repositories {
    /// The repositories whose moves were recorded and have yet to finish.
    pub(super) async fn pending_moves(&self) -> Result<Vec<RepositoryId>> {
        self.store
            .run(|db| {
                Ok(db
                    .prepare("SELECT repository_id FROM repository_moves")?
                    .query_map([], |row| row.get(0))?
                    .collect::<rusqlite::Result<_>>()?)
            })
            .await
    }

    /// Refuse a repository whose records show it in use, as
    /// [`movable_records`] does.
    pub(super) async fn movable_states(&self, id: &RepositoryId, running: Running) -> Result<()> {
        let (id, records) = (id.to_owned(), self.records.clone());
        self.store
            .run(move |db| movable_records(db, &*records, &id, running).map(drop))
            .await
    }

    /// Reserve every workspace of the repository and record the intent, unless
    /// something started using the repository since it was checked.
    pub(super) async fn record_move(
        &self,
        id: &RepositoryId,
        source: &Path,
        destination: &Path,
    ) -> Result<()> {
        let (id, records) = (id.to_owned(), self.records.clone());
        let source_id =
            real_directory_identity(source)?.context("repository directory is missing")?;
        let destination_id = directory_identity(destination)?;
        let claimed = destination.to_owned();
        let source = utf8(source)?;
        let destination = utf8(destination)?;
        self.store
            .write(move |tx| {
                let states = movable_records(tx, &*records, &id, Running::Refused)?;
                // A workspace recorded there since the reservation would be
                // deleted with the destination if the move failed.
                store::ensure_path_unrecorded(tx, &claimed)
                    .context("a workspace was placed where the repository directory would move")?;
                tx.execute(
                    "INSERT INTO repository_moves
                        (repository_id,source,destination,source_id,destination_id,states)
                        VALUES (?1,?2,?3,?4,?5,?6)",
                    params![
                        id,
                        source,
                        destination,
                        source_id,
                        destination_id,
                        serde_json::to_string(&states)?
                    ],
                )?;
                records.reconcile_workspaces(tx, &id)
            })
            .await
    }

    /// Point every record inside the moved directory at the destination.
    pub(super) async fn switch_move(
        &self,
        id: &RepositoryId,
        source: &Path,
        destination: &Path,
    ) -> Result<()> {
        let (id, records) = (id.to_owned(), self.records.clone());
        let (source, destination) = (source.to_owned(), destination.to_owned());
        let (from, to) = (utf8(&source)?, utf8(&destination)?);
        self.store
            .write(move |tx| {
                let inside = |column: &str| {
                    format!("({column}=?2 OR substr({column},1,length(?2)+1)=?2||'/')")
                };
                let rebased = |column: &str| {
                    format!(
                        "CASE WHEN {} THEN ?3||substr({column},length(?2)+1) ELSE {column} END",
                        inside(column)
                    )
                };
                // A local registration's source names its checkout, perhaps
                // spelled differently; a URL source stays.
                tx.execute(
                    &format!(
                        "UPDATE repositories SET path={}, workspaces_dir={},
                            source=CASE WHEN substr(source,1,1)='/' AND {} THEN {} ELSE source END
                            WHERE id=?1",
                        rebased("path"),
                        rebased("workspaces_dir"),
                        inside("path"),
                        rebased("path")
                    ),
                    params![id, from, to],
                )?;
                records.rebase_paths(tx, &id, &source, &destination)?;
                tx.execute(
                    "UPDATE repository_moves SET switched=1 WHERE repository_id=?1",
                    [&id],
                )?;
                Ok(())
            })
            .await
    }

    /// Roll back a move that did not complete, or finish one whose directory
    /// reached its destination. Only directories with their recorded identity
    /// are deleted. Callers hold the repository's Git gate.
    pub(super) async fn finish_move(&self, daemon: &dyn Daemon, id: &RepositoryId) -> Result<()> {
        let Some(intent) = self.move_intent(id).await? else {
            return Ok(());
        };
        if !intent.switched && is_directory(&intent.source, &intent.source_id)? {
            // The rename did not happen or the copy is incomplete.
            remove_recorded(&intent.destination, &intent.destination_id)?;
            return self.close_move(&intent).await;
        }
        if !intent.switched {
            ensure!(
                is_directory(&intent.destination, &intent.source_id)?,
                "repository directory {} is neither in place nor at {}",
                intent.source.display(),
                intent.destination.display()
            );
            self.switch_move(id, &intent.source, &intent.destination)
                .await?;
        }
        // A renamed directory carries the source's identity; a copy fills the
        // reserved one. Without either, the original may be the only copy.
        ensure!(
            is_directory(&intent.destination, &intent.source_id)?
                || is_directory(&intent.destination, &intent.destination_id)?,
            "repository directory {} is unavailable; restore it, then run shoal repo move again",
            intent.destination.display()
        );
        // A copy's original goes first: copied worktrees' Git links still
        // resolve to it, and repair reconnects them only once it is gone.
        remove_recorded(&intent.source, &intent.source_id)?;
        self.reconnect_worktrees(daemon, id).await?;
        // Before the intent goes, so a daemon that stops meanwhile carries
        // the entries when it finishes the move again.
        self.move_agent_trust(daemon, &intent).await;
        self.close_move(&intent).await
    }

    /// Carry the agents' trust entries for the paths the move of `intent`
    /// took to the paths they have now; repeating it changes nothing.
    async fn move_agent_trust(&self, daemon: &dyn Daemon, intent: &Intent) {
        let records = async {
            let repo = self.repository(&intent.repository_id).await?;
            let workspaces = self.repository_workspaces(&repo.id).await?;
            anyhow::Ok((repo, workspaces))
        };
        match records.await {
            Ok((repo, workspaces)) => {
                let moves = moved_paths(intent, &repo, &workspaces);
                daemon.trust_moved(&repo, moves).await;
            }
            Err(error) => log!(
                "agent trust not moved to {}: {error:#}",
                intent.destination.display()
            ),
        }
    }

    /// Repair Git's links between the checkout and its present worktrees,
    /// including the seed's, then verify each workspace still owns its
    /// worktree before recording its admin directory.
    async fn reconnect_worktrees(&self, daemon: &dyn Daemon, id: &RepositoryId) -> Result<()> {
        let repo = self.repository(id).await?;
        let mut present = Vec::new();
        for workspace in self.repository_workspaces(id).await? {
            if off_runtime::exists(&workspace.path).await? {
                present.push(workspace);
            }
        }
        let mut paths: Vec<_> = present.iter().map(|w| w.path.clone()).collect();
        let seed = crate::seed::Layout::of(&repo).worktree();
        if off_runtime::exists(&seed).await? {
            paths.push(seed);
        }
        git::repair_worktrees(&repo.path, &paths).await?;
        for workspace in &present {
            daemon.record_moved_worktree(workspace).await?;
        }
        Ok(())
    }

    async fn move_intent(&self, id: &RepositoryId) -> Result<Option<Intent>> {
        let id = id.to_owned();
        self.store
            .run(move |db| {
                let row = db
                    .query_row(
                        "SELECT source,destination,source_id,destination_id,states,switched
                            FROM repository_moves
                            WHERE repository_id=?1",
                        [&id],
                        |row| {
                            Ok((
                                row.get::<_, String>(0)?,
                                row.get::<_, String>(1)?,
                                row.get(2)?,
                                row.get(3)?,
                                row.get::<_, String>(4)?,
                                row.get(5)?,
                            ))
                        },
                    )
                    .optional()?;
                row.map(
                    |(source, destination, source_id, destination_id, states, switched)| {
                        Ok(Intent {
                            repository_id: id,
                            source: source.into(),
                            destination: destination.into(),
                            source_id,
                            destination_id,
                            states: serde_json::from_str(&states)?,
                            switched,
                        })
                    },
                )
                .transpose()
            })
            .await
    }

    /// Return each workspace to the state it had before the move, and forget
    /// the intent.
    async fn close_move(&self, intent: &Intent) -> Result<()> {
        let (id, records) = (intent.repository_id.clone(), self.records.clone());
        let states = intent.states.clone();
        self.store
            .write(move |tx| {
                for (workspace, state) in states {
                    records.restore_workspace_state(tx, &workspace, state)?;
                }
                tx.execute("DELETE FROM repository_moves WHERE repository_id=?1", [id])?;
                Ok(())
            })
            .await
    }
}

/// The paths of `repo` and its `workspaces` that the move of `intent` took
/// from its source, each as it was there with the one it has now.
fn moved_paths(
    intent: &Intent,
    repo: &Repository,
    workspaces: &[Workspace],
) -> Vec<(PathBuf, PathBuf)> {
    let paths = [&repo.path, &repo.workspaces_dir]
        .into_iter()
        .chain(workspaces.iter().map(|workspace| &workspace.path));
    let mut moves = Vec::new();
    for path in paths {
        let Ok(inside) = path.strip_prefix(&intent.destination) else {
            continue;
        };
        // Joining nothing would add a trailing separator.
        let from = match inside.as_os_str().is_empty() {
            true => intent.source.clone(),
            false => intent.source.join(inside),
        };
        let moved = (from, path.clone());
        if !moves.contains(&moved) {
            moves.push(moved);
        }
    }
    moves
}

fn utf8(path: &Path) -> Result<String> {
    Ok(path
        .to_str()
        .context("repository directory is not UTF-8")?
        .to_owned())
}

/// Refuse a repository whose records show it in use: a lease that handed its
/// checkout to a caller, a command or agent unless `running` lets the caller
/// stop it, a chat, a queued launch or a starting CI job, or a workspace in
/// another lifecycle operation. Returns each workspace's state.
fn movable_records(
    db: &Connection,
    records: &dyn Records,
    id: &RepositoryId,
    running: Running,
) -> Result<BTreeMap<SpaceId, WorkspaceState>> {
    let activity = records.repository_activity(db, id)?;
    ensure!(
        !activity.checkout_leased,
        "the repository checkout is leased as a resource; release it first"
    );
    ensure!(
        running == Running::StoppedFirst || !store::repository_has_executions(db, id)?,
        "a command or agent is running in a workspace of the repository; stop it first"
    );
    ensure!(
        !activity.chats,
        "a chat is running in the repository; wait for it to finish"
    );
    ensure!(
        !activity.launches,
        "agent launches are queued for the repository; wait for it to finish"
    );
    for (workspace, state) in &activity.workspace_states {
        ensure!(
            matches!(state, WorkspaceState::Ready | WorkspaceState::Failed),
            "a workspace of the repository is {state}; wait for it to finish"
        );
        store::ensure_no_pending_rename(db, workspace)?;
    }
    Ok(activity.workspace_states)
}

/// Whether `path` is still the real directory recorded as `identity`.
fn is_directory(path: &Path, identity: &str) -> Result<bool> {
    Ok(is_real_directory(path)? && has_directory_identity(path, identity)?)
}

/// Delete `path` when it is the directory recorded as `identity`, leaving a
/// replacement alone.
fn remove_recorded(path: &Path, identity: &str) -> Result<()> {
    if is_directory(path, identity)? {
        fs::remove_dir_all(path).with_context(|| format!("remove {}", path.display()))?;
    }
    Ok(())
}
