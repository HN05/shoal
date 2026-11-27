//! The preparation of migration 310. It moves the URL of the PR each review
//! covers out of its JSON target into the `target_url` column that
//! migration 309 added.
use anyhow::{Context, Result, bail, ensure};
use rusqlite::{Connection, params};
use serde_json::{Map, Value};

pub(super) fn move_urls(db: &Connection) -> Result<()> {
    let rows = {
        let mut reading = db.prepare("SELECT id,target FROM reviews")?;
        reading
            .query_map([], |row| {
                Ok((row.get::<_, i64>(0)?, row.get::<_, String>(1)?))
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?
    };
    for (id, text) in rows {
        let mut target = serde_json::from_str::<Map<String, Value>>(&text)
            .with_context(|| format!("reviews row {id}: the target is no JSON object"))?;
        let url = take_url(&mut target).with_context(|| format!("reviews row {id}"))?;
        db.execute(
            "UPDATE reviews SET target_url=?2,target=?3 WHERE id=?1",
            params![id, url, serde_json::to_string(&target)?],
        )?;
    }
    Ok(())
}

/// Take the URL out of `target`: a PR's, or none for the workspace.
fn take_url(target: &mut Map<String, Value>) -> Result<Option<String>> {
    match target.get("kind").and_then(Value::as_str) {
        Some("workspace") => {
            ensure!(
                !target.contains_key("url"),
                "a workspace target names a URL"
            );
            Ok(None)
        }
        Some("pr") => match target.remove("url") {
            Some(Value::String(url)) => Ok(Some(url)),
            _ => bail!("a PR target names no URL"),
        },
        _ => bail!("the target has no kind a review takes"),
    }
}
