//! Scope tokens: resolving the one a caller presents, and the tokens a
//! space keeps, which act for it until revoked or the space goes: a chat's
//! own, which its agent carries, and each one a space issues on request. An
//! execution's own token, which the executions table keeps for its
//! wrapper's reattachment, acts for it only while the wrapper is connected.
use super::Caller;
use crate::daemon::workspace::Manager;
use anyhow::Result;
use rusqlite::{Connection, OptionalExtension, params};
use shoal_ids::space_ids::SpaceId;
use shoal_protocol::scope_tokens::ScopeToken;

impl Manager {
    /// The caller `token` acts for: a connected execution, an operation, a
    /// grant's command, or the space that issued it.
    pub(crate) async fn caller(&self, token: &ScopeToken) -> Result<Option<Caller>> {
        if let Some(caller) = self.executions.scope(token).await {
            return Ok(Some(caller));
        }
        if let Some(caller) = self.operations.caller(token) {
            return Ok(Some(caller));
        }
        if let Some(caller) = self.grants.caller(token) {
            return Ok(Some(caller));
        }
        let token = token.to_owned();
        let space = self.store.run(move |db| space(db, &token)).await?;
        Ok(space.map(|space_id| Caller {
            space_id,
            execution: None,
            granted: None,
            read_only: false,
        }))
    }
}

/// The space that issued `token`.
fn space(db: &Connection, token: &ScopeToken) -> Result<Option<SpaceId>> {
    Ok(db
        .query_row(
            "SELECT space_id FROM space_scopes WHERE token=?1",
            [token],
            |row| row.get(0),
        )
        .optional()?)
}

/// Issue a new token that acts for the space `id` until revoked.
pub(crate) fn issue(db: &Connection, id: &SpaceId) -> Result<ScopeToken> {
    let token = super::new_token();
    insert(db, id, &token, false)?;
    Ok(token)
}

/// Issue the token the agent of the chat `id` carries, which lasts as long
/// as the chat.
pub(crate) fn issue_agent(db: &Connection, id: &SpaceId) -> Result<ScopeToken> {
    let token = super::new_token();
    record_agent(db, id, &token)?;
    Ok(token)
}

/// Let `token` act for the agent of the chat `id`.
pub(crate) fn record_agent(db: &Connection, id: &SpaceId, token: &ScopeToken) -> Result<()> {
    insert(db, id, token, true)
}

fn insert(db: &Connection, id: &SpaceId, token: &ScopeToken, agent: bool) -> Result<()> {
    db.execute(
        "INSERT INTO space_scopes(token,space_id,agent) VALUES (?1,?2,?3)",
        params![token, id, agent],
    )?;
    Ok(())
}

/// Revoke the token `token` the space `id` issued, returning whether it
/// issued it; its chat's agent keeps its own.
pub(crate) fn revoke(db: &Connection, id: &SpaceId, token: &ScopeToken) -> Result<bool> {
    let deleted = db.execute(
        "DELETE FROM space_scopes WHERE space_id=?1 AND token=?2 AND NOT agent",
        params![id, token],
    )?;
    Ok(deleted == 1)
}

/// Revoke `token`, which the agent of the chat that became the space `id`
/// carried.
pub(crate) fn revoke_agent(db: &Connection, id: &SpaceId, token: &ScopeToken) -> Result<()> {
    db.execute(
        "DELETE FROM space_scopes WHERE space_id=?1 AND token=?2 AND agent",
        params![id, token],
    )?;
    Ok(())
}

/// Revoke every token of the space `id`.
pub(crate) fn clear(db: &Connection, id: &SpaceId) -> Result<()> {
    db.execute("DELETE FROM space_scopes WHERE space_id=?1", [id])?;
    Ok(())
}

/// The token the agent of the chat `id` carries.
pub(crate) fn agent_token(db: &Connection, id: &SpaceId) -> Result<Option<ScopeToken>> {
    Ok(db
        .query_row(
            "SELECT token FROM space_scopes WHERE space_id=?1 AND agent",
            [id],
            |row| row.get(0),
        )
        .optional()?)
}
