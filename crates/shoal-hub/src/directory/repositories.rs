//! The hub's records of the repositories its machines register.
use anyhow::{Context as _, Result};
use rusqlite::{Transaction, params};
use shoal_ids::member_ids::MemberId;
use shoal_protocol::hub::directory::{ListedRepository, RegisteredRepository};

/// Replace what the hub had of `member`'s repositories with `repositories`.
pub(super) fn replace(
    tx: &Transaction<'_>,
    member: &MemberId,
    repositories: &[RegisteredRepository],
) -> Result<()> {
    forget(tx, member)?;
    for repository in repositories {
        let summary = serde_json::to_string(repository).context("write a repository summary")?;
        tx.execute(
            "INSERT INTO hub_repositories(member_id, repository_id, summary) VALUES (?1,?2,?3)",
            params![member, repository.id, summary],
        )?;
    }
    Ok(())
}

pub(super) fn forget(tx: &Transaction<'_>, member: &MemberId) -> Result<()> {
    tx.execute("DELETE FROM hub_repositories WHERE member_id=?1", [member])?;
    Ok(())
}

/// Every repository in the directory, by machine, then name.
pub(super) fn read(tx: &Transaction<'_>) -> Result<Vec<ListedRepository>> {
    let mut statement =
        tx.prepare("SELECT member_id, summary FROM hub_repositories ORDER BY member_id")?;
    let rows = statement.query_map([], |row| Ok((row.get(0)?, row.get::<_, String>(1)?)))?;
    let mut listed = Vec::new();
    for row in rows {
        let (machine, summary) = row?;
        let repository =
            serde_json::from_str(&summary).context("read a repository summary the hub kept")?;
        listed.push(ListedRepository {
            machine,
            repository,
        });
    }
    listed.sort_by(|a: &ListedRepository, b| {
        let name = |listed: &ListedRepository| listed.repository.repository.name.clone();
        (&a.machine, name(a)).cmp(&(&b.machine, name(b)))
    });
    Ok(listed)
}

#[cfg(test)]
mod tests;
