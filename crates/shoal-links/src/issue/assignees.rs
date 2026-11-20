//! The user's own login among the forge assignees of the issues spaces link
//! as assigned, as the records hold who stands for it and whether Shoal
//! added it.
use anyhow::Result;
use rusqlite::{Connection, OptionalExtension};
use shoal_forge_types::items::reference::ItemRef;
use shoal_ids::space_ids::SpaceId;
use shoal_protocol::model::{Assigner, IssueAssignee};

/// The login a space stood for among an issue's assignees, until it
/// dropped the issue's link.
#[derive(Debug, Clone)]
pub struct Dropped {
    pub url: String,
    pub login: String,
}

/// Record that the space `id` stands for the user's `login` among the
/// assignees of the issue `url`, and with `added` that Shoal added it. A
/// space that no longer links the issue gives the login back to release.
pub fn record_assignee(
    db: &Connection,
    id: &SpaceId,
    url: &str,
    login: &str,
    added: bool,
) -> Result<Option<Dropped>> {
    if added {
        db.execute(
            "INSERT OR IGNORE INTO issue_assignments(url,login) VALUES (?1,?2)",
            rusqlite::params![url, login],
        )?;
    }
    let linked = db.execute(
        "UPDATE space_issue SET assignee=?3 WHERE space_id=?1 AND url=?2",
        rusqlite::params![id, url, login],
    )? == 1;
    Ok((!linked).then(|| Dropped {
        url: url.to_owned(),
        login: login.to_owned(),
    }))
}

/// The logins the space `id` stands for among the assignees of the issues
/// `urls` it links as assigned, which it is about to drop.
pub fn dropped(db: &Connection, id: &SpaceId, urls: &[ItemRef]) -> Result<Vec<Dropped>> {
    let mut statement = db.prepare(
        "SELECT assignee FROM space_issue WHERE space_id=?1 AND url=?2 AND assignee IS NOT NULL",
    )?;
    let mut dropped = Vec::new();
    for url in urls {
        let login: Option<String> =
            (statement.query_row(rusqlite::params![id, url.url()], |row| row.get(0))).optional()?;
        dropped.extend(login.map(|login| Dropped {
            url: url.url().to_owned(),
            login,
        }));
    }
    Ok(dropped)
}

/// Record as Shoal's the user's assignees of the space `id` that
/// `assignees` says Shoal added, as a restored or moved space carries them,
/// for the issues the space links with an assignee again.
pub fn claim(db: &Connection, id: &SpaceId, assignees: &[IssueAssignee]) -> Result<()> {
    let added =
        (assignees.iter()).filter(|assignee| assignee.added && assignee.account == Assigner::User);
    for assignee in added {
        db.execute(
            "INSERT OR IGNORE INTO issue_assignments(url,login)
             SELECT url,assignee FROM space_issue
             WHERE space_id=?1 AND url=?2 AND assignee IS NOT NULL",
            rusqlite::params![id, assignee.url.url()],
        )?;
    }
    Ok(())
}

/// Hand over the user's assignees within the caller's transaction: the
/// space's links no longer say who assigned them, so its agent works none
/// here, and the agent account stays on the issues no other space here
/// links.
pub fn hand_over(db: &Connection, id: &SpaceId) -> Result<()> {
    db.execute(
        "UPDATE space_issue SET linked_by=NULL WHERE space_id=?1",
        [id],
    )?;
    let issues = crate::linked::assigned_urls(db, id)?;
    super::agents::hand_over_agent(db, &issues)?;
    for Dropped { url, login } in dropped(db, id, &issues)? {
        db.execute(
            "UPDATE space_issue SET assignee=NULL WHERE space_id=?1 AND url=?2",
            rusqlite::params![id, url],
        )?;
        take_assignment(db, &url, &login)?;
    }
    Ok(())
}

/// Forget Shoal's assignment of `login` to the issue `url` once no space
/// links the issue for that login, returning whether Shoal had made it.
pub fn take_assignment(db: &Connection, url: &str, login: &str) -> Result<bool> {
    let linked: bool = db.query_row(
        "SELECT EXISTS(SELECT 1 FROM space_issue WHERE url=?1 AND assignee=?2 COLLATE NOCASE)",
        rusqlite::params![url, login],
        |row| row.get(0),
    )?;
    if linked {
        return Ok(false);
    }
    let taken = db.execute(
        "DELETE FROM issue_assignments WHERE url=?1 AND login=?2",
        rusqlite::params![url, login],
    )?;
    Ok(taken == 1)
}
