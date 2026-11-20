//! The rows of changes: recording them in their space's sequence, reading
//! them as protocol records, and the snapshot base and running commands
//! their recording diffs against.
use anyhow::Result;
use rusqlite::{Connection, OptionalExtension as _, Transaction, params};
use shoal_ids::{execution_ids::ExecutionId, repository_ids::RepositoryId, space_ids::SpaceId};
use shoal_protocol::{
    changes::{Change, ChangeAuthor, ChangeKind, Hunk},
    code::{CodeAt, CodeState, Lines},
    item_numbers::ItemNumber,
    space_names::SpaceName,
    threads::ThreadSpace,
};
use shoal_refs::CommitId;

use crate::time::UnixTime;

/// A change to record: who made it, the file and its hunk, or its kind
/// and size for a file without hunks, and the version it produced.
#[derive(Debug, Clone)]
pub(super) struct NewChange {
    pub author: ChangeAuthor,
    pub kind: ChangeKind,
    pub path: String,
    pub hunk: Option<Hunk>,
    pub blob: Option<String>,
    pub size: Option<u64>,
}

/// The space, and HEAD of its worktree, changes are recorded in.
#[derive(Debug, Clone)]
pub(super) struct Made {
    pub space_id: SpaceId,
    pub space_name: SpaceName,
    pub repository_id: RepositoryId,
    pub commit: Option<CommitId>,
    pub at: UnixTime,
}

pub(super) const CHANGES: &str = "SELECT c.id,c.number,c.space_id,
        COALESCE(s.name,c.space_name) AS space_name,s.number AS space_number,c.author,c.kind,
        c.made_at,c.turn,c.execution_id,t.space_number AS thread_space,t.number AS thread_item,
        c.commit_id,c.path,c.start,c.end,c.blob,c.latest_path,c.latest_start,c.latest_end,
        c.latest_blob,c.state,json_array_length(c.added) AS added_count,
        json_array_length(c.removed) AS removed_count,c.size,c.before_start,c.removed,c.added
    FROM space_changes c LEFT JOIN spaces s ON s.id=c.space_id
        LEFT JOIN threads t ON t.id=c.thread_id";

fn lines(row: &rusqlite::Row<'_>, start: &str, end: &str) -> rusqlite::Result<Option<Lines>> {
    Ok(match (row.get(start)?, row.get(end)?) {
        (Some(start), Some(end)) => Some(Lines { start, end }),
        _ => None,
    })
}

pub(super) fn change(row: &rusqlite::Row<'_>) -> rusqlite::Result<Change> {
    let thread = match (row.get("thread_space")?, row.get("thread_item")?) {
        (Some(space), Some(item)) => Some(ItemNumber { space, item }),
        _ => None,
    };
    Ok(Change {
        number: row.get("number")?,
        space: ThreadSpace {
            id: row.get("space_id")?,
            name: row.get("space_name")?,
            number: row.get("space_number")?,
        },
        author: row.get("author")?,
        kind: row.get("kind")?,
        made_at: row.get("made_at")?,
        turn: row.get("turn")?,
        execution: row.get("execution_id")?,
        thread,
        commit: row.get("commit_id")?,
        original: CodeAt {
            path: row.get("path")?,
            lines: lines(row, "start", "end")?,
            blob: row.get("blob")?,
        },
        latest: CodeAt {
            path: row.get("latest_path")?,
            lines: lines(row, "latest_start", "latest_end")?,
            blob: row.get("latest_blob")?,
        },
        state: row.get("state")?,
        added: row.get("added_count")?,
        removed: row.get("removed_count")?,
        size: (row.get::<_, Option<i64>>("size")?).and_then(|size| u64::try_from(size).ok()),
    })
}

/// The changes `filter` keeps, with `params`, in the order they were made.
pub(super) fn query<T>(
    db: &Connection,
    filter: &str,
    params: impl rusqlite::Params,
    map: fn(&rusqlite::Row<'_>) -> rusqlite::Result<T>,
) -> Result<Vec<T>> {
    Ok(db
        .prepare(&format!("{CHANGES} {filter} ORDER BY c.id"))?
        .query_map(params, map)?
        .collect::<rusqlite::Result<_>>()?)
}

/// Record `changes`, made in `made`, each under the next number of its
/// space, with the turn its agent is in, an agent's or mixed one for the
/// thread the agent works for; the changes as recorded.
pub(super) fn insert(
    tx: &Transaction<'_>,
    made: &Made,
    changes: &[NewChange],
) -> Result<Vec<Change>> {
    let (turn, execution) = turn(tx, &made.space_id)?;
    let thread = current_thread(tx, &made.space_id)?;
    let mut ids = Vec::new();
    for new in changes {
        let number = next_number(tx, &made.space_id)?;
        let for_thread = (new.author != ChangeAuthor::Others)
            .then_some(thread)
            .flatten();
        let hunk = new.hunk.as_ref();
        let (start, end) = hunk.map(|hunk| (hunk.lines.start, hunk.lines.end)).unzip();
        tx.execute(
            "INSERT INTO space_changes(space_id,space_name,number,repository_id,author,kind,
                    made_at,turn,execution_id,thread_id,commit_id,path,start,end,blob,size,
                    before_start,removed,added,latest_path,latest_start,latest_end,latest_blob,
                    state)
                VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15,?16,?17,?18,?19,
                    ?12,?13,?14,?15,?20)",
            params![
                made.space_id,
                made.space_name,
                number,
                made.repository_id,
                new.author,
                new.kind,
                made.at,
                turn,
                execution,
                for_thread,
                made.commit,
                new.path,
                start,
                end,
                new.blob,
                new.size.and_then(|size| i64::try_from(size).ok()),
                hunk.map_or(0, |hunk| hunk.before),
                serde_json::to_string(hunk.map_or(&[][..], |hunk| &hunk.removed))?,
                serde_json::to_string(hunk.map_or(&[][..], |hunk| &hunk.added))?,
                CodeState::Unchanged,
            ],
        )?;
        ids.push(tx.last_insert_rowid());
    }
    query(
        tx,
        "WHERE c.id IN (SELECT value FROM json_each(?1))",
        [serde_json::to_string(&ids)?],
        change,
    )
}

/// The next number of the space `space`'s changes, which no change took.
fn next_number(tx: &Transaction<'_>, space: &SpaceId) -> Result<u32> {
    Ok(tx.query_row(
        "INSERT INTO space_change_numbers(space_id,last) VALUES (?1,1)
            ON CONFLICT(space_id) DO UPDATE SET last=last+1 RETURNING last",
        [space],
        |row| row.get(0),
    )?)
}

/// The turn the space `space`'s agent is in, as the space counts them, and
/// its execution.
fn turn(db: &Connection, space: &SpaceId) -> Result<(Option<i64>, Option<ExecutionId>)> {
    Ok(db
        .query_row(
            "SELECT turn,execution_id FROM space_agent_state WHERE space_id=?1",
            [space],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()?
        .unwrap_or_default())
}

/// The thread the space `space`'s agent works for now.
pub(crate) fn current_thread(db: &Connection, space: &SpaceId) -> Result<Option<i64>> {
    Ok(db
        .query_row(
            "SELECT thread_id FROM space_current_thread WHERE space_id=?1",
            [space],
            |row| row.get(0),
        )
        .optional()?)
}

/// The snapshot the space `space`'s next change is diffed against.
pub(super) fn base(db: &Connection, space: &SpaceId) -> Result<Option<String>> {
    Ok(db
        .query_row(
            "SELECT tree FROM space_change_bases WHERE space_id=?1",
            [space],
            |row| row.get(0),
        )
        .optional()?)
}

pub(super) fn set_base(tx: &Transaction<'_>, space: &SpaceId, tree: &str) -> Result<()> {
    tx.execute(
        "INSERT INTO space_change_bases(space_id,tree) VALUES (?1,?2)
            ON CONFLICT(space_id) DO UPDATE SET tree=excluded.tree",
        params![space, tree],
    )?;
    Ok(())
}

/// When each command of the space `space`'s agent that runs now started.
pub(super) fn commands(db: &Connection, space: &SpaceId) -> Result<Vec<(String, UnixTime)>> {
    Ok(db
        .prepare("SELECT call,started_at FROM space_change_commands WHERE space_id=?1")?
        .query_map([space], |row| Ok((row.get(0)?, row.get(1)?)))?
        .collect::<rusqlite::Result<_>>()?)
}

pub(super) fn start_command(
    tx: &Transaction<'_>,
    space: &SpaceId,
    call: &str,
    at: UnixTime,
) -> Result<()> {
    tx.execute(
        "INSERT OR REPLACE INTO space_change_commands(space_id,call,started_at) VALUES (?1,?2,?3)",
        params![space, call, at],
    )?;
    Ok(())
}

pub(super) fn end_command(tx: &Transaction<'_>, space: &SpaceId, call: &str) -> Result<()> {
    tx.execute(
        "DELETE FROM space_change_commands WHERE space_id=?1 AND call=?2",
        params![space, call],
    )?;
    Ok(())
}

/// Forget the commands of the space `space` that still run once its
/// agent's turn ended, which no hook will report the end of.
pub(crate) fn end_commands(tx: &Transaction<'_>, space: &SpaceId) -> Result<()> {
    tx.execute(
        "DELETE FROM space_change_commands WHERE space_id=?1",
        [space],
    )?;
    Ok(())
}

/// The paths of the space `space` an editor reported the user edited at
/// `since` or later.
pub(super) fn edited_since(
    db: &Connection,
    space: &SpaceId,
    since: UnixTime,
) -> Result<Vec<String>> {
    Ok(db
        .prepare("SELECT path FROM space_edit_reports WHERE space_id=?1 AND at>=?2")?
        .query_map(params![space, since], |row| row.get(0))?
        .collect::<rusqlite::Result<_>>()?)
}
