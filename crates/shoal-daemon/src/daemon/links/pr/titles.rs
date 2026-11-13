//! The titles of a space's assigned PRs, which lists and pickers show for
//! them; the space's PR watch record names the PRs.
use std::collections::HashMap;

use anyhow::Result;
use rusqlite::Connection;

use crate::{client::space_ids::SpaceId, forge::items::reference::ItemRef};

/// Record `title` as the assigned PR `url`'s.
pub(super) fn record(db: &Connection, id: &SpaceId, url: &str, title: &str) -> Result<()> {
    db.execute(
        "INSERT INTO space_pr_titles(space_id,url,title) VALUES (?1,?2,?3)
         ON CONFLICT(space_id,url) DO UPDATE SET title=excluded.title",
        rusqlite::params![id, url, title],
    )?;
    Ok(())
}

/// Record `title`, which the forge reports for `url`, as its title when the
/// space assigns that PR.
pub(in crate::daemon::links) fn refresh(
    db: &Connection,
    id: &SpaceId,
    url: &str,
    title: &str,
) -> Result<()> {
    if super::watched_prs(db, id)?.iter().any(|pr| pr.url() == url) {
        record(db, id, url, title)?;
    }
    Ok(())
}

/// Forget the titles of the PRs `urls`, which the space no longer assigns.
pub(super) fn forget(db: &Connection, id: &SpaceId, urls: &[ItemRef]) -> Result<()> {
    for url in urls {
        db.execute(
            "DELETE FROM space_pr_titles WHERE space_id=?1 AND url=?2",
            rusqlite::params![id, url.url()],
        )?;
    }
    Ok(())
}

/// The titles of the space's assigned PRs by URL.
pub(crate) fn of_space(db: &Connection, id: &SpaceId) -> rusqlite::Result<HashMap<String, String>> {
    db.prepare("SELECT url,title FROM space_pr_titles WHERE space_id=?1")?
        .query_map([id], |row| Ok((row.get(0)?, row.get(1)?)))?
        .collect()
}
