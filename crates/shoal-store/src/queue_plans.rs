//! The plans of queued launches, rewritten into the one shape every kind
//! of space queues: the launch of the agent of the space the entry names,
//! and the PR it links as it starts.
use anyhow::Result;
use rusqlite::{Connection, params};
use serde_json::{Map, Value, json};

/// Rewrite the plan of each queued launch, a workspace's or a chat's, into
/// a launch and the PR it links.
pub(super) fn one_queue_plan(db: &Connection) -> Result<()> {
    let launches = db
        .prepare(
            "SELECT id,plan,issue_title,issue_labels,source FROM agent_queue
             WHERE kind='launch' AND json_valid(plan)",
        )?
        .query_map([], |row| {
            Ok(Queued {
                id: row.get(0)?,
                plan: row.get(1)?,
                issue_title: row.get(2)?,
                issue_labels: row.get(3)?,
                source: row.get(4)?,
            })
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    for queued in launches {
        if let Some(plan) = queued.rewritten()? {
            db.execute(
                "UPDATE agent_queue SET plan=?2 WHERE id=?1",
                params![queued.id, plan.to_string()],
            )?;
        }
    }
    Ok(())
}

/// A queued launch as its row recorded it.
struct Queued {
    id: String,
    plan: String,
    issue_title: Option<String>,
    issue_labels: Option<String>,
    source: Option<String>,
}

impl Queued {
    /// Its plan in the one shape, unless it has it already.
    fn rewritten(&self) -> Result<Option<Value>> {
        let Value::Object(plan) = serde_json::from_str(&self.plan)? else {
            return Ok(None);
        };
        if plan.contains_key("launch") {
            return Ok(None);
        }
        let (mut launch, pr) = match plan.get("chat_start") {
            Some(chat) => (chat_launch(&chat["resolved"]), Value::Null),
            None => (self.workspace_launch(&plan), field(&plan, "pr").clone()),
        };
        if let Some(source) = &self.source {
            launch.insert("source".into(), serde_json::from_str(source)?);
        }
        let mut rewritten = Map::from_iter([("launch".to_owned(), Value::Object(launch))]);
        if !pr.is_null() {
            rewritten.insert("pr".into(), pr);
        }
        Ok(Some(Value::Object(rewritten)))
    }

    /// The launch a workspace's `plan` queued, with the issue its row
    /// titles.
    fn workspace_launch(&self, plan: &Map<String, Value>) -> Map<String, Value> {
        let agent = field(plan, "agent");
        let mut launch = fields(plan, ["prompt", "mode", "narrowing", "fork"]);
        launch.insert("agent".into(), agent["agent"].clone());
        launch.insert("model".into(), agent["model"].clone());
        launch.insert("args".into(), Value::Array(args(field(plan, "args"))));
        if let Some(url) = field(plan, "issue").as_str() {
            let labels = (self.issue_labels.as_deref())
                .and_then(|labels| serde_json::from_str(labels).ok())
                .unwrap_or(json!([]));
            let title = self.issue_title.as_deref().unwrap_or(url);
            launch.insert(
                "issue".into(),
                json!({"url": url, "title": title, "labels": labels}),
            );
        }
        without_nulls(launch)
    }
}

/// The launch a chat's resolved `start` queued, on the issue it names,
/// titled as its links have it.
fn chat_launch(start: &Value) -> Map<String, Value> {
    let Value::Object(start) = start else {
        return Map::new();
    };
    let mut launch = fields(
        start,
        [
            "agent",
            "mode",
            "model",
            "narrowing",
            "prompt",
            "args",
            "now",
        ],
    );
    if let Some(url) = start.get("issue").and_then(Value::as_str) {
        let linked = (start.get("links"))
            .and_then(|links| links["issues"].as_array())
            .and_then(|issues| issues.iter().find(|issue| issue["url"] == url));
        let title = (linked.and_then(|issue| issue["title"].as_str())).unwrap_or(url);
        launch.insert("issue".into(), json!({"url": url, "title": title}));
    }
    without_nulls(launch)
}

/// The `keys` of `record` it holds.
fn fields<const N: usize>(record: &Map<String, Value>, keys: [&str; N]) -> Map<String, Value> {
    (keys.into_iter())
        .filter_map(|key| Some((key.to_owned(), record.get(key)?.clone())))
        .collect()
}

/// The value `record` holds under `key`, null when it holds none.
fn field<'a>(record: &'a Map<String, Value>, key: &str) -> &'a Value {
    record.get(key).unwrap_or(&Value::Null)
}

/// The arguments `args` held as the platform's strings, as text.
fn args(args: &Value) -> Vec<Value> {
    let Some(args) = args.as_array() else {
        return Vec::new();
    };
    (args.iter())
        .map(|arg| match arg {
            Value::Object(arg) => {
                let bytes = (arg.get("Unix").and_then(Value::as_array).into_iter())
                    .flatten()
                    .filter_map(|byte| u8::try_from(byte.as_u64()?).ok())
                    .collect::<Vec<_>>();
                Value::String(String::from_utf8_lossy(&bytes).into_owned())
            }
            arg => arg.clone(),
        })
        .collect()
}

/// `launch` without the fields it holds no value for.
fn without_nulls(mut launch: Map<String, Value>) -> Map<String, Value> {
    launch.retain(|_, value| !value.is_null());
    launch
}

#[cfg(test)]
mod tests;
