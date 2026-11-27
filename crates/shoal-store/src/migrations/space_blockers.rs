//! The fill of migration 334: each blocker in a plan's JSON moves into a
//! row of `space_blockers`.
use anyhow::Result;
use rusqlite::{Connection, params};
use serde::Deserialize;

use super::json_rows::JsonRow;

/// A blocker as plans stored it.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Blocker {
    kind: String,
    target: String,
    name: String,
    reason: String,
    #[serde(default)]
    lookup_failed: Option<String>,
    #[serde(default)]
    step: Option<Step>,
}

#[derive(Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
enum Step {
    Key(String),
    Name(String),
}

pub(super) fn move_blockers(db: &Connection) -> Result<()> {
    let rows = db
        .prepare("SELECT space_id,record FROM space_progress")?
        .query_map([], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    for (id, text) in rows {
        let mut plan = JsonRow::parse("space_progress", id.as_str(), &text)?;
        let blockers: Vec<Blocker> = plan.take_optional(&["blockers"])?.unwrap_or_default();
        for (position, blocker) in (0_i64..).zip(blockers) {
            let (key, step) = match blocker.step {
                Some(Step::Key(key)) => (Some(key), None),
                Some(Step::Name(name)) => (None, Some(name)),
                None => (None, None),
            };
            db.execute(
                "INSERT INTO space_blockers(space_id,position,kind,target,name,reason,
                    lookup_failed,step_key,step_name) VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9)",
                params![
                    id,
                    position,
                    blocker.kind,
                    blocker.target,
                    blocker.name,
                    blocker.reason,
                    blocker.lookup_failed,
                    key,
                    step
                ],
            )?;
        }
        db.execute(
            "UPDATE space_progress SET record=?2 WHERE space_id=?1",
            params![id, plan.text()],
        )?;
    }
    Ok(())
}
