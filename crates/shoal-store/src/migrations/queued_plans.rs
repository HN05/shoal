//! The preparation of migration 337. A launch's plan, in the agent queue
//! and among failed launches, no longer holds what the row's columns do: its
//! launch's agent, mode, permissions and source, and its issue's title and
//! labels, which leave only the issue's URL in the plan.
use anyhow::{Context, Result, bail};
use rusqlite::{Connection, params};
use serde_json::{Map, Value};

/// The tables whose rows plan a launch.
const TABLES: &[&str] = &["agent_queue", "failed_launches"];

/// The fields of a plan's launch that the row's columns hold instead, which
/// a launch gives only when they are not empty.
const OPTIONAL_COPIES: &[&str] = &["mode", "narrowing", "source"];

pub(super) fn drop_column_copies(db: &Connection) -> Result<()> {
    for table in TABLES {
        for (id, plan) in plans(db, table)? {
            let rewritten = rewrite(&plan).with_context(|| format!("{table} row {id}"))?;
            db.execute(
                &format!(
                    "UPDATE {table} SET plan=?2,
                        issue_title=coalesce(?3,issue_title),issue_labels=coalesce(?4,issue_labels)
                     WHERE id=?1"
                ),
                params![id, rewritten.plan, rewritten.title, rewritten.labels],
            )?;
        }
    }
    Ok(())
}

/// The ID and plan of each row of `table` that plans a launch.
fn plans(db: &Connection, table: &str) -> Result<Vec<(String, String)>> {
    let mut reading = db.prepare(&format!(
        "SELECT id,plan FROM {table} WHERE plan IS NOT NULL"
    ))?;
    let rows = reading.query_map([], |row| Ok((row.get(0)?, row.get(1)?)))?;
    Ok(rows.collect::<rusqlite::Result<_>>()?)
}

/// A plan without the fields its row's columns hold, with the title and
/// labels of the issue it named, which those columns take.
struct Rewritten {
    plan: String,
    title: Option<String>,
    labels: Option<String>,
}

fn rewrite(plan: &str) -> Result<Rewritten> {
    let Value::Object(mut plan) = serde_json::from_str(plan).context("read its plan")? else {
        bail!("its plan is not an object");
    };
    let Some(Value::Object(launch)) = plan.get_mut("launch") else {
        bail!("its plan has no launch");
    };
    launch
        .remove("agent")
        .context("its plan's launch names no agent")?;
    for field in OPTIONAL_COPIES {
        launch.remove(*field);
    }
    let (title, labels) = match launch.remove("issue") {
        None | Some(Value::Null) => (None, None),
        Some(Value::Object(issue)) => {
            let (url, title, labels) = issue_parts(issue)?;
            launch.insert("issue".into(), url);
            (Some(title), Some(labels.to_string()))
        }
        Some(_) => bail!("its plan's issue is not an object"),
    };
    Ok(Rewritten {
        plan: Value::Object(plan).to_string(),
        title,
        labels,
    })
}

/// The URL, title and labels of a plan's `issue`.
fn issue_parts(mut issue: Map<String, Value>) -> Result<(Value, String, Value)> {
    let url = (issue.remove("url"))
        .filter(Value::is_string)
        .context("its plan's issue has no URL")?;
    let title = match issue.remove("title") {
        Some(Value::String(title)) => title,
        _ => bail!("its plan's issue has no title"),
    };
    let labels = match issue.remove("labels") {
        None => Value::Array(Vec::new()),
        Some(labels @ Value::Array(_)) => labels,
        Some(_) => bail!("its plan's issue labels are not a list"),
    };
    Ok((url, title, labels))
}
