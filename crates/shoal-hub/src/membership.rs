//! This machine's record of the hub it joined.
use anyhow::Result;
use rusqlite::{OptionalExtension as _, Transaction, params};
use shoal_ids::member_ids::MemberId;
use shoal_protocol::hub::{Membership, PublicKey, Route};

use crate::identity::KeyPair;

/// The membership as the store holds it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Joined {
    pub membership: Membership,
    pub key: KeyPair,
    pub hub_key: PublicKey,
    /// The hub revoked it, so this machine no longer connects.
    pub revoked: bool,
}

pub fn load(tx: &Transaction<'_>) -> Result<Option<Joined>> {
    Ok(tx
        .query_row(
            "SELECT member_id, name, private_key, hub_key, url, connect, joined_at, revoked
                FROM hub_membership WHERE id=1",
            [],
            |row| {
                Ok(Joined {
                    membership: Membership {
                        member: row.get("member_id")?,
                        name: row.get("name")?,
                        through: through(row)?,
                        joined_at: row.get("joined_at")?,
                    },
                    key: KeyPair::from(row.get::<_, Vec<u8>>("private_key")?),
                    hub_key: row.get("hub_key")?,
                    revoked: row.get("revoked")?,
                })
            },
        )
        .optional()?)
}

/// The route a membership's `row` joined through, its URL or command.
fn through(row: &rusqlite::Row<'_>) -> rusqlite::Result<Route> {
    let connect: Option<String> = row.get("connect")?;
    match connect {
        Some(command) => serde_json::from_str(&command)
            .map(Route::Command)
            .map_err(|error| {
                rusqlite::Error::FromSqlConversionFailure(
                    0,
                    rusqlite::types::Type::Text,
                    error.into(),
                )
            }),
        None => row.get("url").map(Route::Url),
    }
}

/// Keep `joined` as this machine's membership, replacing any other.
pub fn save(tx: &Transaction<'_>, joined: &Joined) -> Result<()> {
    let Membership {
        member,
        name,
        through,
        joined_at,
    } = &joined.membership;
    let (url, connect) = match through {
        Route::Url(url) => (Some(url), None),
        Route::Command(command) => (None, Some(serde_json::to_string(command)?)),
    };
    tx.execute(
        "INSERT OR REPLACE INTO hub_membership(
            id, member_id, name, private_key, hub_key, url, connect, joined_at, revoked
        ) VALUES (1,?1,?2,?3,?4,?5,?6,?7,?8)",
        params![
            member,
            name,
            joined.key.as_bytes(),
            joined.hub_key,
            url,
            connect,
            joined_at,
            joined.revoked
        ],
    )?;
    Ok(())
}

/// Record that the hub revoked the membership of `member`, which a later
/// join may have replaced.
pub fn revoked(tx: &Transaction<'_>, member: &MemberId) -> Result<()> {
    tx.execute(
        "UPDATE hub_membership SET revoked=1 WHERE id=1 AND member_id=?1",
        [member],
    )?;
    Ok(())
}

pub fn forget(tx: &Transaction<'_>) -> Result<()> {
    tx.execute("DELETE FROM hub_membership", [])?;
    Ok(())
}
