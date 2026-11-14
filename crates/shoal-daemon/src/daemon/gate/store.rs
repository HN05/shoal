//! The gate's records: a session per space and subject, and the verdicts it
//! gave each action, which go with the space.
use anyhow::Result;
use rusqlite::{Connection, OptionalExtension, params};

use crate::client::{approvals::GateVerdict, space_ids::SpaceId, time::UnixTime};
use crate::{agent::Agent, agent::AgentSessionId};

/// A subject's saved session.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Session {
    pub agent: Agent,
    /// The rules its opening gave it, as JSON.
    pub rules: String,
    pub session: AgentSessionId,
    /// The commit its last verdict saw, for a subject that follows commits.
    pub seen: Option<String>,
}

/// The session of `space` for `subject`.
pub(crate) fn session(db: &Connection, space: &SpaceId, subject: &str) -> Result<Option<Session>> {
    Ok(db
        .query_row(
            "SELECT agent,rules,session,seen FROM gate_sessions WHERE space_id=?1 AND subject=?2",
            params![space, subject],
            |row| {
                Ok(Session {
                    agent: Agent::unchecked(row.get(0)?),
                    rules: row.get(1)?,
                    session: AgentSessionId::from(row.get::<_, String>(2)?),
                    seen: row.get(3)?,
                })
            },
        )
        .optional()?)
}

/// Keep `session` as the one of `space` for `subject`, in place of any other.
pub(crate) fn save_session(
    db: &Connection,
    space: &SpaceId,
    subject: &str,
    session: &Session,
    now: UnixTime,
) -> Result<()> {
    db.execute(
        "INSERT INTO gate_sessions(space_id,subject,agent,rules,session,seen,updated_at)
         VALUES (?1,?2,?3,?4,?5,?6,?7)
         ON CONFLICT(space_id,subject) DO UPDATE SET agent=?3,rules=?4,session=?5,seen=?6,
             updated_at=?7",
        params![
            space,
            subject,
            session.agent.name(),
            session.rules,
            session.session.as_str(),
            session.seen,
            now
        ],
    )?;
    Ok(())
}

/// Record the gate's verdict on `action` of `space`.
pub(crate) fn record_verdict(
    db: &Connection,
    space: &SpaceId,
    action: &str,
    verdict: GateVerdict,
    reason: &str,
    now: UnixTime,
) -> Result<()> {
    db.execute(
        "INSERT INTO gate_verdicts(space_id,action,verdict,reason,created_at)
         VALUES (?1,?2,?3,?4,?5)",
        params![space, action, verdict.as_str(), reason, now],
    )?;
    Ok(())
}

/// How many times the gate denied `action` of `space`, failures aside, with
/// the reason of the latest denial.
pub(crate) fn denials(db: &Connection, space: &SpaceId, action: &str) -> Result<(u32, String)> {
    Ok(db.query_row(
        "SELECT COUNT(*),COALESCE((SELECT reason FROM gate_verdicts
             WHERE space_id=?1 AND action=?2 AND verdict='deny' ORDER BY id DESC LIMIT 1),'')
         FROM gate_verdicts WHERE space_id=?1 AND action=?2 AND verdict='deny'",
        params![space, action],
        |row| Ok((row.get(0)?, row.get(1)?)),
    )?)
}

#[cfg(test)]
mod tests;
