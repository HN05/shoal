//! The files a thread's post sent, which `thread_post_files` lists by
//! their records, in the order the post sent them.
use std::collections::HashMap;

use anyhow::Result;
use rusqlite::{Connection, Transaction, params};
use shoal_protocol::files::{KeptCopy, SentFile};

/// The files the posts `posts` sent, by post, each post's in order. A file
/// whose record is gone, as after `shoal files rm`, drops out.
pub(super) fn of_posts(db: &Connection, posts: &[i64]) -> Result<HashMap<i64, Vec<SentFile>>> {
    let found = db
        .prepare(
            "SELECT p.post_id,f.id,f.space_id,f.name,f.title,f.path
                FROM thread_post_files p JOIN space_files f ON f.id=p.file_id
                WHERE p.post_id IN (SELECT value FROM json_each(?1))
                ORDER BY p.post_id,p.position",
        )?
        .query_map([serde_json::to_string(posts)?], |row| {
            let file = SentFile {
                id: row.get("id")?,
                space: row.get("space_id")?,
                name: row.get("name")?,
                title: row.get("title")?,
                copy: KeptCopy::Here {
                    path: row.get::<_, String>("path")?.into(),
                },
            };
            Ok((row.get::<_, i64>("post_id")?, file))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    let mut by_post: HashMap<i64, Vec<SentFile>> = HashMap::new();
    for (post, file) in found {
        by_post.entry(post).or_default().push(file);
    }
    Ok(by_post)
}

/// The record IDs of the files the post `post` sent, in order, those whose
/// record is gone included.
pub(super) fn ids(db: &Connection, post: i64) -> Result<Vec<i64>> {
    Ok(db
        .prepare("SELECT file_id FROM thread_post_files WHERE post_id=?1 ORDER BY position")?
        .query_map([post], |row| row.get(0))?
        .collect::<rusqlite::Result<_>>()?)
}

/// Have the post `post` name the files whose records are `files`, in that
/// order, in place of those it named.
pub(super) fn set(tx: &Transaction<'_>, post: i64, files: &[i64]) -> Result<()> {
    tx.execute("DELETE FROM thread_post_files WHERE post_id=?1", [post])?;
    for (position, file) in (0_i64..).zip(files) {
        tx.execute(
            "INSERT INTO thread_post_files(post_id,position,file_id) VALUES (?1,?2,?3)",
            params![post, position, file],
        )?;
    }
    Ok(())
}

#[cfg(test)]
mod tests;
