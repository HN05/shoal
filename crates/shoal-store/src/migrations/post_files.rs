//! The preparation of migration 324. It lists the files each thread's
//! post sent in the table migration 323 made, in the order of the post's
//! JSON list of their record IDs.
use anyhow::{Context, Result};
use rusqlite::{Connection, params};

pub(super) fn list_post_files(db: &Connection) -> Result<()> {
    for (post, files) in posts_with_files(db)? {
        let files: Vec<i64> = serde_json::from_str(&files)
            .with_context(|| format!("thread_posts row {post}: its files are no list of IDs"))?;
        insert_files(db, post, &files)?;
    }
    Ok(())
}

/// Each post that sent files, with its JSON list of their record IDs.
fn posts_with_files(db: &Connection) -> Result<Vec<(i64, String)>> {
    Ok(db
        .prepare("SELECT id,files FROM thread_posts WHERE files<>'[]'")?
        .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))?
        .collect::<rusqlite::Result<_>>()?)
}

/// List the files `files` of the post `post`, in their order.
fn insert_files(db: &Connection, post: i64, files: &[i64]) -> Result<()> {
    for (position, file) in (0_i64..).zip(files) {
        db.execute(
            "INSERT INTO thread_post_files(post_id,position,file_id) VALUES (?1,?2,?3)",
            params![post, position, file],
        )?;
    }
    Ok(())
}
