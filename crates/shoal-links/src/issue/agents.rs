//! The agent account among the forge assignees of the issues spaces link as
//! assigned, as the records hold which spaces' agents assigned them and
//! where this Shoal put the account on.
use anyhow::Result;
use rusqlite::{Connection, params};
use shoal_forge_types::items::reference::ItemRef;
use shoal_ids::space_ids::SpaceId;
use shoal_protocol::model::Assigner;

/// A space that links an issue as assigned, and who assigned it there.
pub struct Linker {
    pub id: SpaceId,
    pub assigner: Assigner,
}

/// The issues the space `id` links as assigned whose agent account it
/// settles here: all but those it handed to the machine it moved to.
pub fn settled_here(db: &Connection, id: &SpaceId) -> Result<Vec<ItemRef>> {
    Ok(db
        .prepare(
            "SELECT url FROM space_issue WHERE space_id=?1 AND linked_by IS NOT NULL
             ORDER BY rowid",
        )?
        .query_map([id], |row| row.get(0))?
        .collect::<rusqlite::Result<_>>()?)
}

/// Record that an agent of the space `id` ended: the issues an agent assigned
/// it count as worked only while it has an agent again.
pub fn agent_ended(db: &Connection, id: &SpaceId) -> Result<()> {
    db.execute(
        "UPDATE space_issue SET linked_by=?2 WHERE space_id=?1 AND linked_by=?3",
        params![id, Assigner::User, Assigner::Agent],
    )?;
    Ok(())
}

/// Record that this Shoal put or kept the agent account on the issue `url`.
pub fn record_agent_assigned(db: &Connection, url: &str) -> Result<()> {
    db.execute(
        "INSERT OR IGNORE INTO agent_assigned_issues(url) VALUES (?1)",
        [url],
    )?;
    Ok(())
}

/// Whether this Shoal put or kept the agent account on the issue `url`.
pub fn agent_assigned_here(db: &Connection, url: &str) -> Result<bool> {
    Ok(db.query_row(
        "SELECT EXISTS(SELECT 1 FROM agent_assigned_issues WHERE url=?1)",
        [url],
        |row| row.get(0),
    )?)
}

/// Forget that this Shoal put the agent account on the issue `url`.
pub fn forget_agent_assigned(db: &Connection, url: &str) -> Result<()> {
    db.execute("DELETE FROM agent_assigned_issues WHERE url=?1", [url])?;
    Ok(())
}

/// Leave the agent account on `issues`, which a space that moved to
/// another machine links, to that machine, where no space here settles it.
pub fn hand_over_agent(db: &Connection, issues: &[ItemRef]) -> Result<()> {
    for url in issues {
        db.execute(
            "DELETE FROM agent_assigned_issues WHERE url=?1 AND NOT EXISTS(
                SELECT 1 FROM space_issue WHERE url=?1 AND linked_by IS NOT NULL)",
            [url.url()],
        )?;
    }
    Ok(())
}

/// The spaces here that link the issue `url` as assigned.
pub fn linkers(db: &Connection, url: &str) -> Result<Vec<Linker>> {
    Ok(db
        .prepare(
            "SELECT space_id,linked_by FROM space_issue WHERE url=?1 AND linked_by IS NOT NULL",
        )?
        .query_map([url], |row| {
            Ok(Linker {
                id: row.get(0)?,
                assigner: row.get(1)?,
            })
        })?
        .collect::<rusqlite::Result<_>>()?)
}
