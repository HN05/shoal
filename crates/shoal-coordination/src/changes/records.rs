//! The rows of changes, and the snapshot base and running commands their
//! recording diffs against.
use std::collections::HashMap;

use anyhow::Result;
use rusqlite::{Connection, OptionalExtension as _, Transaction, params};
use shoal_ids::{repository_ids::RepositoryId, space_ids::SpaceId};
use shoal_protocol::{
    changes::{Change, ChangeAuthor, ChangeDiff, ChangeEntry, ChangeKind, Counted, Hunk},
    code::{CodeAt, CodeState, Lines},
    item_numbers::ItemNumber,
    space_names::SpaceName,
    threads::ThreadSpace,
};
use shoal_refs::CommitId;

use crate::daemon::Records;
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
        c.latest_blob,c.state,c.added_count,c.removed_count,c.size,c.before_start,c.removed,
        c.added
    FROM space_changes c LEFT JOIN spaces s ON s.id=c.space_id
        LEFT JOIN threads t ON t.id=c.thread_id";

fn lines(row: &rusqlite::Row<'_>, start: &str, end: &str) -> rusqlite::Result<Option<Lines>> {
    Ok(match (row.get(start)?, row.get(end)?) {
        (Some(start), Some(end)) => Some(Lines { start, end }),
        _ => None,
    })
}

pub fn change(row: &rusqlite::Row<'_>) -> rusqlite::Result<Change> {
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

pub(super) fn diff(row: &rusqlite::Row<'_>) -> rusqlite::Result<ChangeDiff> {
    Ok(ChangeDiff {
        change: change(row)?,
        before: row.get("before_start")?,
        removed_lines: (shoal_store::json_column(row, "removed")?).unwrap_or_default(),
        added_lines: (shoal_store::json_column(row, "added")?).unwrap_or_default(),
        cut: false,
    })
}

/// The changes `filter` keeps, with `params`, in the order they were made.
pub fn query<T>(
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
    outside: &dyn Records,
    made: &Made,
    changes: &[NewChange],
) -> Result<Vec<Change>> {
    let (turn, execution) = outside.agent_turn(tx, &made.space_id)?;
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
                    before_start,removed,added,removed_count,added_count,latest_path,
                    latest_start,latest_end,latest_blob,state)
                VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15,?16,?17,?18,?19,
                    ?20,?21,?12,?13,?14,?15,?22)",
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
                hunk.map_or(0, |hunk| hunk.removed.len() as i64),
                hunk.map_or(0, |hunk| hunk.added.len() as i64),
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

/// The thread the space `space`'s agent works for now.
pub fn current_thread(db: &Connection, space: &SpaceId) -> Result<Option<i64>> {
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

pub fn set_base(tx: &Transaction<'_>, space: &SpaceId, tree: &str) -> Result<()> {
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
pub fn end_commands(tx: &Transaction<'_>, space: &SpaceId) -> Result<()> {
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

/// How many of a space's latest changes a listing of it returns.
const LATEST: u32 = 200;

/// The latest changes of the space `space`, oldest first.
pub(super) fn latest<T>(
    db: &Connection,
    space: &SpaceId,
    map: fn(&rusqlite::Row<'_>) -> rusqlite::Result<T>,
) -> Result<Vec<T>> {
    query(
        db,
        "WHERE c.id IN (SELECT id FROM space_changes WHERE space_id=?1 ORDER BY id DESC LIMIT ?2)",
        params![space, LATEST],
        map,
    )
}

/// The changes of the thread `thread`, made at `since` or later.
pub(super) fn of_thread<T>(
    db: &Connection,
    thread: i64,
    since: Option<UnixTime>,
    map: fn(&rusqlite::Row<'_>) -> rusqlite::Result<T>,
) -> Result<Vec<T>> {
    let since = since.unwrap_or(UnixTime::from_seconds(0));
    query(
        db,
        "WHERE c.thread_id=?1 AND c.made_at>=?2",
        params![thread, since],
        map,
    )
}

/// What change entries count of a change, without its lines.
const COUNTED: &str = "SELECT c.thread_id,c.number,c.space_id,
        COALESCE(s.name,c.space_name) AS space_name,s.number AS space_number,c.kind,c.made_at,
        c.turn,c.execution_id,c.latest_path,c.added_count,c.removed_count
    FROM space_changes c LEFT JOIN spaces s ON s.id=c.space_id";

fn counted(row: &rusqlite::Row<'_>) -> rusqlite::Result<(i64, Counted)> {
    let counted = Counted {
        number: row.get("number")?,
        space: ThreadSpace {
            id: row.get("space_id")?,
            name: row.get("space_name")?,
            number: row.get("space_number")?,
        },
        kind: row.get("kind")?,
        made_at: row.get("made_at")?,
        turn: row.get("turn")?,
        execution: row.get("execution_id")?,
        path: row.get("latest_path")?,
        added: row.get("added_count")?,
        removed: row.get("removed_count")?,
    };
    Ok((row.get("thread_id")?, counted))
}

/// The change entries of each of the threads `threads` that has changes,
/// split where one of its posts, made at the times `posts` gives by
/// thread, falls inside a turn.
pub fn entries(
    db: &Connection,
    threads: &[i64],
    posts: &HashMap<i64, Vec<UnixTime>>,
) -> Result<HashMap<i64, Vec<ChangeEntry>>> {
    let found = db
        .prepare(&format!(
            "{COUNTED} WHERE c.thread_id IN (SELECT value FROM json_each(?1)) ORDER BY c.id"
        ))?
        .query_map([serde_json::to_string(threads)?], counted)?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    let mut by_thread: HashMap<i64, Vec<Counted>> = HashMap::new();
    for (thread, change) in found {
        by_thread.entry(thread).or_default().push(change);
    }
    Ok((by_thread.into_iter())
        .map(|(thread, changes)| {
            let times = posts.get(&thread).map_or(&[][..], Vec::as_slice);
            (thread, shoal_protocol::changes::entries(changes, times))
        })
        .collect())
}

/// The changes whose IDs are `ids`.
pub(super) fn of_ids(db: &Connection, ids: &[i64]) -> Result<Vec<Change>> {
    query(
        db,
        "WHERE c.id IN (SELECT value FROM json_each(?1))",
        [serde_json::to_string(ids)?],
        change,
    )
}

/// The ID of each change `named` names by its space and number, and the
/// thread it is for, refusing one that does not exist.
pub(super) fn named(db: &Connection, named: &[(SpaceId, u32)]) -> Result<Vec<(i64, Option<i64>)>> {
    let mut found = Vec::new();
    for (space, number) in named {
        let row = db
            .query_row(
                "SELECT id,thread_id FROM space_changes WHERE space_id=?1 AND number=?2",
                params![space, number],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()?;
        found.push(row.ok_or_else(|| anyhow::anyhow!("no change {number} in that space"))?);
    }
    Ok(found)
}

/// Record that the changes `ids` were made by `author`.
pub(super) fn set_author(tx: &Transaction<'_>, ids: &[i64], author: ChangeAuthor) -> Result<()> {
    tx.execute(
        "UPDATE space_changes SET author=?2 WHERE id IN (SELECT value FROM json_each(?1))",
        params![serde_json::to_string(ids)?, author],
    )?;
    Ok(())
}

/// Record that the changes `ids` are for the thread `thread`, or none.
pub(super) fn set_thread(tx: &Transaction<'_>, ids: &[i64], thread: Option<i64>) -> Result<()> {
    tx.execute(
        "UPDATE space_changes SET thread_id=?2 WHERE id IN (SELECT value FROM json_each(?1))",
        params![serde_json::to_string(ids)?, thread],
    )?;
    Ok(())
}

/// Record that the space `space`'s agent works for the thread `thread`,
/// or none.
pub(super) fn switch(tx: &Transaction<'_>, space: &SpaceId, thread: Option<i64>) -> Result<()> {
    match thread {
        Some(thread) => tx.execute(
            "INSERT INTO space_current_thread(space_id,thread_id) VALUES (?1,?2)
                ON CONFLICT(space_id) DO UPDATE SET thread_id=excluded.thread_id",
            params![space, thread],
        )?,
        None => tx.execute(
            "DELETE FROM space_current_thread WHERE space_id=?1",
            [space],
        )?,
    };
    Ok(())
}

/// Record that the post `post` points at the changes `named` names.
pub fn point(tx: &Transaction<'_>, post: i64, named: &[(SpaceId, u32)]) -> Result<()> {
    for (id, _) in self::named(tx, named)? {
        tx.execute(
            "INSERT OR IGNORE INTO thread_post_changes(post_id,change_id) VALUES (?1,?2)",
            params![post, id],
        )?;
    }
    Ok(())
}

/// The changes each of the posts `posts` points at, by post, with their
/// lines up to a snippet's length.
pub fn of_posts(db: &Connection, posts: &[i64]) -> Result<HashMap<i64, Vec<ChangeDiff>>> {
    let pointed =
        |row: &rusqlite::Row<'_>| Ok((row.get::<_, i64>("post_id")?, diff(row)?.cut_to_snippet()));
    let joined = CHANGES.replace(
        "FROM space_changes c",
        ",p.post_id FROM thread_post_changes p JOIN space_changes c ON c.id=p.change_id",
    );
    let found = db
        .prepare(&format!(
            "{joined} WHERE p.post_id IN (SELECT value FROM json_each(?1)) ORDER BY c.id"
        ))?
        .query_map([serde_json::to_string(posts)?], pointed)?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    let mut by_post: HashMap<i64, Vec<ChangeDiff>> = HashMap::new();
    for (post, diff) in found {
        by_post.entry(post).or_default().push(diff);
    }
    Ok(by_post)
}

/// Delete the changes of the repository `repository` made before `before`
/// that neither a thread nor a post keeps, as checkpoints expire.
pub fn expire(db: &Connection, repository: &RepositoryId, before: UnixTime) -> Result<()> {
    db.execute(
        "DELETE FROM space_changes WHERE repository_id=?1 AND thread_id IS NULL AND made_at<?2
            AND id NOT IN (SELECT change_id FROM thread_post_changes)",
        params![repository, before],
    )?;
    Ok(())
}

/// Move the lines of the change `number` of the space `space` to `latest`
/// in `state`, unless they were followed from `from` meanwhile; whether
/// they moved.
pub fn follow(
    tx: &Transaction<'_>,
    space: &SpaceId,
    number: u32,
    from: &CodeAt,
    latest: &CodeAt,
    state: CodeState,
) -> Result<bool> {
    let (start, end) = latest.lines.map(|lines| (lines.start, lines.end)).unzip();
    Ok(tx.execute(
        "UPDATE space_changes SET latest_path=?5,latest_start=?6,latest_end=?7,latest_blob=?8,
                state=?9
            WHERE space_id=?1 AND number=?2 AND latest_path=?3 AND latest_blob IS ?4",
        params![
            space,
            number,
            from.path,
            from.blob,
            latest.path,
            start,
            end,
            latest.blob,
            state
        ],
    )? > 0)
}

/// Record that an editor reported the user edited the file `path` of the
/// space `space`'s worktree at `at`.
pub(super) fn report_edit(
    tx: &Transaction<'_>,
    space: &SpaceId,
    path: &str,
    at: UnixTime,
) -> Result<()> {
    tx.execute(
        "INSERT INTO space_edit_reports(space_id,path,at) VALUES (?1,?2,?3)
            ON CONFLICT(space_id,path) DO UPDATE SET at=excluded.at",
        params![space, path, at],
    )?;
    Ok(())
}
