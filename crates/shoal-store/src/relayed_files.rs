//! The files of the space summaries the hub keeps, which other machines
//! reported: each thread post's file names the machine that keeps it, and
//! the space it is attached to, in place of a path on that machine. A post
//! kept its files in its sender's space where that machine ran the sender,
//! else in its thread's space.
use std::collections::{HashMap, HashSet};

use anyhow::Result;
use rusqlite::{Connection, params};
use serde_json::{Value, json};

/// Rewrite the summaries of `hub_spaces` and `hub_space_copies`, each
/// reported by the member that names the machine keeping its files.
pub(super) fn summaries_name_their_machine(db: &Connection) -> Result<()> {
    let ran = spaces_by_member(db)?;
    let none = HashSet::new();
    for table in ["hub_spaces", "hub_space_copies"] {
        let summaries = db
            .prepare(&format!(
                "SELECT s.rowid,s.summary,s.member_id,m.name FROM {table} s
                    LEFT JOIN hub_members m ON m.id=s.member_id"
            ))?
            .query_map([], |row| {
                Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?))
            })?
            .collect::<rusqlite::Result<Vec<(i64, String, String, Option<String>)>>>()?;
        for (rowid, summary, member, machine) in summaries {
            let mut summary: Value = serde_json::from_str(&summary)?;
            let reporter = Reporter {
                machine: &machine.unwrap_or_default(),
                spaces: ran.get(&member).unwrap_or(&none),
            };
            if reporter.kept_on(&mut summary) {
                let update = format!("UPDATE {table} SET summary=?2 WHERE rowid=?1");
                db.execute(&update, params![rowid, summary.to_string()])?;
            }
        }
    }
    Ok(())
}

/// The spaces the hub lists each member running, by member ID.
fn spaces_by_member(db: &Connection) -> Result<HashMap<String, HashSet<String>>> {
    let mut ran: HashMap<String, HashSet<String>> = HashMap::new();
    let rows = db
        .prepare("SELECT member_id,space_id FROM hub_spaces")?
        .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))?
        .collect::<rusqlite::Result<Vec<(String, String)>>>()?;
    for (member, space) in rows {
        ran.entry(member).or_default().insert(space);
    }
    Ok(ran)
}

/// The machine that reported a summary, which keeps its posts' files, and
/// the spaces it runs.
struct Reporter<'a> {
    machine: &'a str,
    spaces: &'a HashSet<String>,
}

impl Reporter<'_> {
    /// Name the reporter as keeping the files of `summary`'s thread posts,
    /// each in the space that keeps it; whether any file changed.
    fn kept_on(&self, summary: &mut Value) -> bool {
        let mut changed = false;
        for thread in elements(summary, "threads") {
            let numbered = thread.get("space_id").cloned().unwrap_or(Value::Null);
            for post in elements(thread, "posts") {
                let space = self.keeping_space(post, &numbered);
                for file in elements(post, "files").filter_map(Value::as_object_mut) {
                    if file.remove("path").is_some() {
                        file.insert("machine".into(), json!(self.machine));
                        file.insert("space".into(), space.clone());
                        changed = true;
                    }
                }
            }
        }
        changed
    }

    /// The space that keeps `post`'s files: its sender's where the
    /// reporter runs it, else the thread's, `numbered`.
    fn keeping_space(&self, post: &Value, numbered: &Value) -> Value {
        let sender = post.pointer("/space/id").and_then(Value::as_str);
        match sender.filter(|sender| self.spaces.contains(*sender)) {
            Some(sender) => json!(sender),
            None => numbered.clone(),
        }
    }
}

/// The elements of the array `value` holds at `key`, none without one.
fn elements<'a>(value: &'a mut Value, key: &str) -> impl Iterator<Item = &'a mut Value> {
    (value.get_mut(key).and_then(Value::as_array_mut).into_iter()).flatten()
}
