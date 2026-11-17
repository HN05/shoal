//! This machine's record of the hub it joined: the ID the hub gave it, the
//! hub's key every link checks, and the URL it paired at.
use anyhow::Result;
use rusqlite::{OptionalExtension as _, Transaction, params};
use shoal_protocol::hub::{Membership, PublicKey};

/// The membership as the store holds it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Joined {
    pub membership: Membership,
    pub hub_key: PublicKey,
    /// The hub revoked it, so this machine no longer connects.
    pub revoked: bool,
}

pub(super) fn load(tx: &Transaction<'_>) -> Result<Option<Joined>> {
    Ok(tx
        .query_row(
            "SELECT member_id, name, hub_key, url, joined_at, revoked FROM hub_membership WHERE id=1",
            [],
            |row| {
                Ok(Joined {
                    membership: Membership {
                        member: row.get("member_id")?,
                        name: row.get("name")?,
                        url: row.get("url")?,
                        joined_at: row.get("joined_at")?,
                    },
                    hub_key: row.get("hub_key")?,
                    revoked: row.get("revoked")?,
                })
            },
        )
        .optional()?)
}

/// Keep `joined` as this machine's membership, replacing any other.
pub(super) fn save(tx: &Transaction<'_>, joined: &Joined) -> Result<()> {
    let Membership {
        member,
        name,
        url,
        joined_at,
    } = &joined.membership;
    tx.execute(
        "INSERT OR REPLACE INTO hub_membership(id, member_id, name, hub_key, url, joined_at, revoked)
            VALUES (1,?1,?2,?3,?4,?5,?6)",
        params![member, name, joined.hub_key, url, joined_at, joined.revoked],
    )?;
    Ok(())
}

/// Record that the hub revoked the membership.
pub(super) fn revoked(tx: &Transaction<'_>) -> Result<()> {
    tx.execute("UPDATE hub_membership SET revoked=1 WHERE id=1", [])?;
    Ok(())
}

/// Forget the membership.
pub(super) fn forget(tx: &Transaction<'_>) -> Result<()> {
    tx.execute("DELETE FROM hub_membership", [])?;
    Ok(())
}
