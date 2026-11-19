//! The hub's record of its members: the machines and devices paired with it,
//! the hub's own machine among them, with the key each proves and when each
//! last connected.
use anyhow::{Result, bail};
use rusqlite::{OptionalExtension as _, Row, Transaction, params};
use shoal_ids::member_ids::MemberId;
use shoal_protocol::hub::{
    Member, MemberKind, PublicKey,
    availability::{AvailabilitySetting, Description},
};

use crate::refusal::{Reason, Refusal};
use crate::time::UnixTime;

/// A member as the store holds it.
#[derive(Debug)]
pub(super) struct Recorded {
    pub member: Member,
    pub key: PublicKey,
    pub revoked: bool,
}

const COLUMNS: &str = "id, kind, name, public_key, joined_at, version, last_seen, revoked_at";

fn recorded(row: &Row<'_>, own: &PublicKey) -> rusqlite::Result<Recorded> {
    let key: PublicKey = row.get("public_key")?;
    Ok(Recorded {
        member: Member {
            id: row.get("id")?,
            kind: row.get("kind")?,
            name: row.get("name")?,
            version: row.get("version")?,
            joined_at: row.get("joined_at")?,
            last_seen: row.get("last_seen")?,
            connected: false,
            hub: &key == own,
            availability: None,
            agents: Vec::new(),
        },
        revoked: row.get::<_, Option<UnixTime>>("revoked_at")?.is_some(),
        key,
    })
}

/// Record the hub's own machine, holding `own`, named `name`, unless it is.
pub(super) fn record_own(tx: &Transaction<'_>, own: &PublicKey, name: &str) -> Result<()> {
    let recorded: bool = tx.query_row(
        "SELECT EXISTS(SELECT 1 FROM hub_members WHERE public_key=?1)",
        [own],
        |row| row.get(0),
    )?;
    if !recorded {
        join(tx, MemberKind::Machine, name, own)?;
    }
    Ok(())
}

/// Record a new member of `kind` named `name`, which proves `key`, under a
/// new ID.
pub(super) fn join(
    tx: &Transaction<'_>,
    kind: MemberKind,
    name: &str,
    key: &PublicKey,
) -> Result<MemberId> {
    let taken: bool = tx.query_row(
        "SELECT EXISTS(SELECT 1 FROM hub_members WHERE public_key=?1)",
        [key],
        |row| row.get(0),
    )?;
    if taken {
        bail!("this key is paired already; a joining machine or device makes a key of its own");
    }
    let id = MemberId::generate();
    tx.execute(
        "INSERT INTO hub_members(id, kind, name, public_key, joined_at) VALUES (?1,?2,?3,?4,?5)",
        params![id, kind, name, key, UnixTime::now()],
    )?;
    Ok(id)
}

/// The member `id`, revoked or not.
pub(super) fn find(
    tx: &Transaction<'_>,
    id: &MemberId,
    own: &PublicKey,
) -> Result<Option<Recorded>> {
    let query = format!("SELECT {COLUMNS} FROM hub_members WHERE id=?1");
    Ok(tx
        .query_row(&query, [id], |row| recorded(row, own))
        .optional()?)
}

/// Every member the hub has not revoked, its own machine first.
pub(super) fn list(tx: &Transaction<'_>, own: &PublicKey) -> Result<Vec<Member>> {
    let query = format!(
        "SELECT {COLUMNS} FROM hub_members WHERE revoked_at IS NULL
            ORDER BY public_key=?1 DESC, joined_at, id"
    );
    let mut statement = tx.prepare(&query)?;
    let members = statement.query_map([own], |row| recorded(row, own))?;
    Ok(members
        .map(|member| member.map(|recorded| recorded.member))
        .collect::<rusqlite::Result<_>>()?)
}

/// The member that `selector`, an ID or a name only one member has, names
/// among those the hub has not revoked.
pub(super) fn select(tx: &Transaction<'_>, selector: &str, own: &PublicKey) -> Result<Member> {
    let members = list(tx, own)?;
    if let Some(member) = members.iter().find(|member| member.id.as_str() == selector) {
        return Ok(member.clone());
    }
    let mut named = members.into_iter().filter(|member| member.name == selector);
    match (named.next(), named.next()) {
        (Some(member), None) => Ok(member),
        (Some(_), Some(_)) => Err(Refusal::new(
            Reason::NotFound,
            format!("several members are named {selector}; name one by the ID shoal hub ls shows"),
        )
        .into()),
        (None, _) => Err(Refusal::new(
            Reason::NotFound,
            format!("no machine or device of the hub is {selector}; shoal hub ls lists them"),
        )
        .into()),
    }
}

/// Revoke `id`'s credential, which its links then cannot open.
pub(super) fn revoke(tx: &Transaction<'_>, id: &MemberId) -> Result<()> {
    tx.execute(
        "UPDATE hub_members SET revoked_at=?2 WHERE id=?1 AND revoked_at IS NULL",
        params![id, UnixTime::now()],
    )?;
    Ok(())
}

/// Record that `id` connected now, or its link ended now, with `version`.
pub(super) fn seen(tx: &Transaction<'_>, id: &MemberId, version: &str) -> Result<()> {
    tx.execute(
        "UPDATE hub_members SET last_seen=?2, version=?3 WHERE id=?1",
        params![id, UnixTime::now(), version],
    )?;
    Ok(())
}

/// Whether `id` names a member the hub has not revoked.
pub(super) fn admitted(tx: &Transaction<'_>, id: &MemberId) -> Result<bool> {
    Ok(tx.query_row(
        "SELECT EXISTS(SELECT 1 FROM hub_members WHERE id=?1 AND revoked_at IS NULL)",
        [id],
        |row| row.get(0),
    )?)
}

/// Record what the machine `id` says of itself.
pub(super) fn describe(
    tx: &Transaction<'_>,
    id: &MemberId,
    description: Description,
) -> Result<()> {
    tx.execute(
        "UPDATE hub_members SET battery=?2, availability=?3 WHERE id=?1",
        params![id, description.battery, description.availability],
    )?;
    Ok(())
}

/// What the machine `id` last said of itself: whether it has a battery,
/// none before it said, and its setting.
pub(super) fn description(
    tx: &Transaction<'_>,
    id: &MemberId,
) -> Result<(Option<bool>, AvailabilitySetting)> {
    Ok(tx.query_row(
        "SELECT battery, availability FROM hub_members WHERE id=?1",
        [id],
        |row| Ok((row.get(0)?, row.get(1)?)),
    )?)
}
