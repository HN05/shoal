//! Rewrite the JSON records the store holds into the shapes this release
//! reads, a migration per change of shape, so code reads one shape of each.
use anyhow::Result;
use rusqlite::{Connection, params};
use serde_json::{Map, Value, json};

/// Watches name when the space last changed the item `own_change_at`,
/// removal records keep every last session in `last_sessions`, and an
/// agent's terminal place is its `origin`.
pub(super) fn renamed_fields(db: &Connection) -> Result<()> {
    rewrite(db, "pr_activity", |watch| {
        renamed(watch, "own_state_at", "own_change_at")
    })?;
    rewrite(db, "removed_spaces", |removed| {
        if let Some(last) = removed
            .remove("last_session")
            .filter(|last| !last.is_null())
        {
            (removed.entry("last_sessions")).or_insert_with(|| Value::Array(vec![last]));
        }
        for agent in objects(removed, "agents") {
            if let Some(record) = agent.get_mut("record").and_then(Value::as_object_mut) {
                origin_from_place(record);
            }
        }
        for last in objects(removed, "last_sessions") {
            origin_from_place(last);
        }
    })
}

/// Fill what records an earlier Shoal wrote go without: a simulator's
/// acquirer, the user's; a stopped agent's mode, the default one; a last
/// session's execution, a new one; who attached a file, the user; and a
/// link event's relation, an assignment.
pub(super) fn filled_fields(db: &Connection) -> Result<()> {
    rewrite(db, "simulators", |simulator| {
        filled(simulator, "acquirer", || json!({"kind": "user"}))
    })?;
    rewrite(db, "removed_spaces", |removed| {
        for agent in objects(removed, "agents") {
            if let Some(record) = agent.get_mut("record").and_then(Value::as_object_mut) {
                filled(record, "mode", default_mode);
            }
        }
        for last in objects(removed, "last_sessions") {
            filled(last, "mode", default_mode);
            filled(last, "execution", || {
                json!(shoal_ids::execution_ids::ExecutionId::generate())
            });
        }
    })?;
    db.execute_batch(
        r#"UPDATE space_files SET attached_by='{"kind":"user"}' WHERE attached_by IS NULL;
        UPDATE space_events
            SET record=json_set(record,'$.file.attached_by',json('{"kind":"user"}'))
            WHERE json_valid(record) AND json_type(record,'$.file') IS NOT NULL
                AND coalesce(json_type(record,'$.file.attached_by'),'null')='null';
        UPDATE space_events SET record=json_set(record,'$.link.relation','assigned')
            WHERE json_valid(record) AND json_type(record,'$.link') IS NOT NULL
                AND json_type(record,'$.link.relation') IS NULL;"#,
    )?;
    Ok(())
}

fn default_mode() -> Value {
    json!(shoal_protocol::config::modes::ModeName::DEFAULT)
}

/// Removal records list their assigned issues and PRs as items, where
/// earlier ones held one issue with its title beside the list and their PRs'
/// bare URLs; PR registrations list their watched URLs, where earlier ones
/// named a single PR; and snapshots keep a comment's author beside its
/// fingerprint, and each reviewer's verdicts apart.
pub(super) fn listed_items(db: &Connection) -> Result<()> {
    rewrite(db, "removed_spaces", |removed| {
        if let Some(links) = removed.get_mut("links").and_then(Value::as_object_mut) {
            listed_links(links);
        }
    })?;
    rewrite(db, "pr_cleanup", |registration| {
        if let Some(url) = registration.remove("url").filter(|url| !url.is_null()) {
            (registration.entry("urls")).or_insert_with(|| Value::Array(vec![url]));
        }
    })?;
    for table in ["pr_activity", "space_item_activity"] {
        rewrite(db, table, |snapshot| {
            if let Some(comments) = snapshot.get_mut("comments").and_then(Value::as_object_mut) {
                authored_comments(comments);
            }
        })?;
    }
    Ok(())
}

/// `links` with its single issue first among its issues and each PR an
/// item.
fn listed_links(links: &mut Map<String, Value>) {
    let title = links.remove("issue_title").unwrap_or(Value::Null);
    let earlier = (links.remove("issue").filter(|url| !url.is_null()))
        .map(|url| json!({"kind": "issue", "url": url, "title": title}));
    for key in ["issues", "prs", "opened", "related"] {
        filled(links, key, || json!([]));
    }
    if let Some(Value::Array(issues)) = links.get_mut("issues") {
        issues.splice(0..0, earlier);
    }
    if let Some(Value::Array(prs)) = links.get_mut("prs") {
        for pr in prs.iter_mut().filter(|pr| pr.is_string()) {
            *pr = json!({"kind": "pr", "url": pr.take()});
        }
    }
}

/// The key under which earlier snapshots kept every reviewer's verdict as
/// one entry, a `reviewer:verdict` line each.
const COMBINED_VERDICTS: &str = "reviews:verdicts";

/// `comments` with each bare fingerprint an entry of an unknown author,
/// and combined verdicts an entry for each reviewer with theirs, sorted.
fn authored_comments(comments: &mut Map<String, Value>) {
    for entry in comments.values_mut().filter(|entry| entry.is_string()) {
        *entry = json!({"fingerprint": entry.take()});
    }
    let Some(combined) = comments.remove(COMBINED_VERDICTS) else {
        return;
    };
    let lines = combined["fingerprint"].as_str().unwrap_or_default().lines();
    let mut by_reviewer = std::collections::BTreeMap::<&str, Vec<&str>>::new();
    for (reviewer, verdict) in lines.filter_map(|line| line.rsplit_once(':')) {
        by_reviewer.entry(reviewer).or_default().push(verdict);
    }
    for (reviewer, mut verdicts) in by_reviewer {
        verdicts.sort();
        comments.insert(
            format!("reviews:verdict:{reviewer}"),
            json!({"fingerprint": verdicts.join(","), "author": reviewer}),
        );
    }
}

/// Events earlier versions journaled take the shapes recorded now: a
/// withdrawn completion is `undone`, a chat opening a workspace or
/// returning it is `parent_changed` naming the chat as its `parent`, a
/// parent workspace is the `parent`, and a ready mark names no commit.
pub(super) fn current_events(db: &Connection) -> Result<()> {
    let chats = chat_names(db)?;
    rewrite(db, "space_events", |event| {
        match event.get("kind").and_then(Value::as_str) {
            Some("continued") => {
                event.insert("kind".into(), json!("undone"));
            }
            Some("chat_changed") => {
                event.insert("kind".into(), json!("parent_changed"));
                let chat = event
                    .remove("chat")
                    .and_then(|chat| chat.as_str().map(str::to_owned));
                let parent = chat.map(|id| {
                    let name = chats.get(&id).cloned().unwrap_or_else(|| id.clone());
                    json!({"id": id, "name": name, "kind": "chat"})
                });
                event.insert("parent".into(), parent.unwrap_or(Value::Null));
            }
            _ => {}
        }
        if let Some(workspace) = event.remove("parent_workspace") {
            let parent = match workspace {
                Value::Object(mut workspace) => {
                    workspace.insert("kind".into(), json!("workspace"));
                    Value::Object(workspace)
                }
                none => none,
            };
            event.entry("parent").or_insert(parent);
        }
        if let Some(review) = event.get_mut("review").and_then(Value::as_object_mut) {
            review.remove("head");
        }
    })
}

/// The name of every chat, removed or not, by its ID.
fn chat_names(db: &Connection) -> Result<std::collections::HashMap<String, String>> {
    Ok(db
        .prepare(
            "SELECT id,name FROM spaces WHERE kind='chat'
             UNION ALL SELECT space_id,name FROM removed_spaces WHERE chat",
        )?
        .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))?
        .collect::<rusqlite::Result<_>>()?)
}

/// Each workspace's Git directory, and each repository directory a
/// removal deletes, takes the identity named by its birth time where an
/// earlier version recorded its `device:inode`.
pub(super) fn birth_identities(db: &Connection) -> Result<()> {
    birth_identities_of(
        db,
        "SELECT id,git_dir,git_dir_id FROM workspaces
         WHERE git_dir IS NOT NULL AND git_dir_id NOT LIKE 'birth:%'",
        "UPDATE workspaces SET git_dir_id=?2 WHERE id=?1",
    )?;
    birth_identities_of(
        db,
        "SELECT repository_id,path,directory_id FROM repository_removals
             JOIN repositories ON repositories.id=repository_id
         WHERE directory_id NOT LIKE 'birth:%'",
        "UPDATE repository_removals SET directory_id=?2 WHERE repository_id=?1",
    )
}

/// Give each directory `select` names, as its key, path and recorded
/// identity, its birth identity through `update`.
fn birth_identities_of(db: &Connection, select: &str, update: &str) -> Result<()> {
    let recorded = db
        .prepare(select)?
        .query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
            ))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    for (id, git_dir, identity) in recorded {
        let path = std::path::Path::new(&git_dir);
        if let Some(birth) = crate::daemon::workspace::identity::birth_identity(path, &identity) {
            db.execute(update, params![id, birth])?;
        }
    }
    Ok(())
}

/// Rewrite every record of `table` with `change`, writing back those it
/// changed.
fn rewrite(db: &Connection, table: &str, change: impl Fn(&mut Map<String, Value>)) -> Result<()> {
    let records = db
        .prepare(&format!(
            "SELECT rowid,record FROM {table} WHERE json_valid(record)"
        ))?
        .query_map([], |row| {
            Ok((row.get::<_, i64>(0)?, row.get::<_, String>(1)?))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    for (rowid, text) in records {
        let Value::Object(mut record) = serde_json::from_str(&text)? else {
            continue;
        };
        let before = record.clone();
        change(&mut record);
        if record != before {
            db.execute(
                &format!("UPDATE {table} SET record=?2 WHERE rowid=?1"),
                params![rowid, Value::Object(record).to_string()],
            )?;
        }
    }
    Ok(())
}

/// The objects of the array `record` holds under `key`.
fn objects<'a>(
    record: &'a mut Map<String, Value>,
    key: &str,
) -> impl Iterator<Item = &'a mut Map<String, Value>> {
    (record
        .get_mut(key)
        .and_then(Value::as_array_mut)
        .into_iter())
    .flatten()
    .filter_map(Value::as_object_mut)
}

/// Move `record`'s field `old` to `new`, unless it has `new` already.
fn renamed(record: &mut Map<String, Value>, old: &str, new: &str) {
    if let Some(value) = record.remove(old) {
        record.entry(new).or_insert(value);
    }
}

/// Give `record` the field `key`, as `value` makes it, unless it has one.
fn filled(record: &mut Map<String, Value>, key: &str, value: impl FnOnce() -> Value) {
    if record.get(key).is_none_or(Value::is_null) {
        record.insert(key.to_owned(), value());
    }
}

/// An agent's `place` as its `origin`, where a bare pane ID, which only
/// Herdr recorded, becomes a Herdr origin.
fn origin_from_place(record: &mut Map<String, Value>) {
    let Some(place) = record.remove("place") else {
        return;
    };
    let origin = match place {
        Value::String(pane) => json!({"host": "herdr", "place": pane}),
        origin => origin,
    };
    record.entry("origin").or_insert(origin);
}

#[cfg(test)]
mod tests;
