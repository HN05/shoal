//! The rows of code references: reading them as protocol records,
//! recording one, moving one to where it was followed, and the file
//! versions a space's references keep.
use std::collections::HashMap;

use anyhow::Result;
use rusqlite::{Connection, Transaction, params};
use shoal_ids::{repository_ids::RepositoryId, space_ids::SpaceId};
use shoal_protocol::{
    code::{CodeAt, CodeRef, CodeState, Lines},
    threads::ThreadSpace,
};

use super::NewCode;

const CODE: &str = "SELECT c.id,c.post_id,c.space_id,COALESCE(s.name,c.space_name) AS space_name,
        s.number AS space_number,c.commit_id,c.path,c.start,c.end,c.blob,c.snippet,c.latest_path,
        c.latest_start,c.latest_end,c.latest_blob,c.state
    FROM thread_code c LEFT JOIN spaces s ON s.id=c.space_id";

/// The lines the columns `start` and `end` hold, none for a whole file.
fn lines(row: &rusqlite::Row<'_>, start: &str, end: &str) -> rusqlite::Result<Option<Lines>> {
    Ok(match (row.get(start)?, row.get(end)?) {
        (Some(start), Some(end)) => Some(Lines { start, end }),
        _ => None,
    })
}

fn code(row: &rusqlite::Row<'_>) -> rusqlite::Result<(i64, CodeRef)> {
    let code = CodeRef {
        id: row.get("id")?,
        space: ThreadSpace {
            id: row.get("space_id")?,
            name: row.get("space_name")?,
            number: row.get("space_number")?,
        },
        commit: row.get("commit_id")?,
        original: CodeAt {
            path: row.get("path")?,
            lines: lines(row, "start", "end")?,
            blob: row.get("blob")?,
        },
        snippet: row.get("snippet")?,
        latest: CodeAt {
            path: row.get("latest_path")?,
            lines: lines(row, "latest_start", "latest_end")?,
            blob: row.get("latest_blob")?,
        },
        state: row.get("state")?,
    };
    Ok((row.get("post_id")?, code))
}

/// The references of the posts `posts`, by post, each post's in order.
pub(in crate::daemon::threads) fn of_posts(
    db: &Connection,
    posts: &[i64],
) -> Result<HashMap<i64, Vec<CodeRef>>> {
    let found = db
        .prepare(&format!(
            "{CODE} WHERE c.post_id IN (SELECT value FROM json_each(?1)) ORDER BY c.id"
        ))?
        .query_map([serde_json::to_string(posts)?], code)?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    let mut by_post: HashMap<i64, Vec<CodeRef>> = HashMap::new();
    for (post, code) in found {
        by_post.entry(post).or_default().push(code);
    }
    Ok(by_post)
}

/// Record `new` as a reference of the post `post`, followed to where it was
/// made.
pub(in crate::daemon::threads) fn insert(
    tx: &Transaction<'_>,
    post: i64,
    new: &NewCode,
) -> Result<()> {
    let (start, end) = new.at.lines.map(|lines| (lines.start, lines.end)).unzip();
    tx.execute(
        "INSERT INTO thread_code(post_id,space_id,space_name,repository_id,commit_id,path,start,
                end,blob,snippet,latest_path,latest_start,latest_end,latest_blob,state)
            VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?6,?7,?8,?9,?11)",
        params![
            post,
            new.space_id,
            new.space_name,
            new.repository_id,
            new.commit,
            new.at.path,
            start,
            end,
            new.at.blob,
            new.snippet,
            CodeState::Unchanged
        ],
    )?;
    Ok(())
}

/// Move the reference `id` to `latest` in `state`, unless it was followed
/// from `from` meanwhile; whether it moved.
pub(super) fn follow(
    tx: &Transaction<'_>,
    id: i64,
    from: &CodeAt,
    latest: &CodeAt,
    state: CodeState,
) -> Result<bool> {
    let (start, end) = latest.lines.map(|lines| (lines.start, lines.end)).unzip();
    Ok(tx.execute(
        "UPDATE thread_code SET latest_path=?4,latest_start=?5,latest_end=?6,latest_blob=?7,
                state=?8
            WHERE id=?1 AND latest_path=?2 AND latest_blob IS ?3",
        params![
            id,
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

/// Every file version the references in the worktree of the space `space`
/// keep.
pub(super) fn kept(db: &Connection, space: &SpaceId) -> Result<Vec<String>> {
    Ok(db
        .prepare(
            "SELECT blob FROM thread_code WHERE space_id=?1 AND blob IS NOT NULL
            UNION SELECT latest_blob FROM thread_code WHERE space_id=?1 AND latest_blob IS NOT NULL
            ORDER BY 1",
        )?
        .query_map([space], |row| row.get(0))?
        .collect::<rusqlite::Result<_>>()?)
}

/// The spaces whose references keep file versions in the repository
/// `repository`.
pub(super) fn spaces_in(db: &Connection, repository: &RepositoryId) -> Result<Vec<SpaceId>> {
    Ok(db
        .prepare("SELECT DISTINCT space_id FROM thread_code WHERE repository_id=?1")?
        .query_map([repository], |row| row.get(0))?
        .collect::<rusqlite::Result<_>>()?)
}
