//! Rewrite the JSON records the store holds into the shapes this release
//! reads, a migration per change of shape, so code reads one shape of each.
use std::{collections::HashMap, fs, path::Path};

use anyhow::Result;
use rusqlite::{Connection, params};
use serde_json::{Map, Value, json};

use crate::fsutil::atomic::{Permissions, ReplaceOptions, replace_atomically};

/// Watches name when the space last changed the item `own_change_at`,
/// removal records keep every last session in `last_sessions`, and an
/// agent's terminal place is its `origin`.
pub(super) fn renamed_fields(db: &Connection) -> Result<()> {
    rewrite(db, "pr_activity", |watch| {
        renamed(watch, "own_state_at", "own_change_at")
    })?;
    rewrite(db, "removed_spaces", |removed| {
        listed(removed, "last_session", "last_sessions");
        for_each_agent(removed, origin_from_place);
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
        for_each_agent(removed, |agent| filled(agent, "mode", default_mode));
        for last in objects(removed, "last_sessions") {
            filled(last, "execution", || {
                json!(shoal_ids::execution_ids::ExecutionId::generate())
            });
        }
    })?;
    db.execute_batch(USER_ATTACHED_FILES)?;
    db.execute_batch(ASSIGNED_LINK_EVENTS)?;
    Ok(())
}

/// Files and their events that name no attacher were the user's.
const USER_ATTACHED_FILES: &str = r#"
    UPDATE space_files SET attached_by='{"kind":"user"}' WHERE attached_by IS NULL;
    UPDATE space_events
        SET record=json_set(record,'$.file.attached_by',json('{"kind":"user"}'))
        WHERE json_valid(record) AND json_type(record,'$.file') IS NOT NULL
            AND coalesce(json_type(record,'$.file.attached_by'),'null')='null';"#;

/// Files and their events that name a chat as their attacher name the user
/// instead: every agent attaches as an agent now, a chat's as a
/// workspace's, by its execution, which those records never named.
pub(super) const USER_CHAT_ATTACHERS: &str = r#"
    UPDATE space_files SET attached_by='{"kind":"user"}'
        WHERE json_valid(attached_by) AND json_extract(attached_by,'$.kind')='chat';
    UPDATE space_events
        SET record=json_set(record,'$.file.attached_by',json('{"kind":"user"}'))
        WHERE json_valid(record)
            AND json_extract(record,'$.file.attached_by.kind')='chat';"#;

/// Link events without a relation were assignments.
const ASSIGNED_LINK_EVENTS: &str = "
    UPDATE space_events SET record=json_set(record,'$.link.relation','assigned')
        WHERE json_valid(record) AND json_type(record,'$.link') IS NOT NULL
            AND json_type(record,'$.link.relation') IS NULL;";

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
        listed(registration, "url", "urls")
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
            Some("chat_changed") => parent_from_chat(event, &chats),
            _ => {}
        }
        parent_from_workspace(event);
        if let Some(review) = event.get_mut("review").and_then(Value::as_object_mut) {
            review.remove("head");
        }
    })
}

/// A `chat_changed` event as the `parent_changed` event naming its chat,
/// by the name `chats` holds for it, or none once it returned to the user.
fn parent_from_chat(event: &mut Map<String, Value>, chats: &HashMap<String, String>) {
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

/// An event's `parent_workspace` as its `parent`.
fn parent_from_workspace(event: &mut Map<String, Value>) {
    let Some(workspace) = event.remove("parent_workspace") else {
        return;
    };
    let parent = match workspace {
        Value::Object(mut workspace) => {
            workspace.insert("kind".into(), json!("workspace"));
            Value::Object(workspace)
        }
        none => none,
    };
    event.entry("parent").or_insert(parent);
}

/// The name of every chat, removed or not, by its ID.
fn chat_names(db: &Connection) -> Result<HashMap<String, String>> {
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

/// The files of stopped agents and last sessions in the state directory
/// beside the database take the shapes their removal records' copies took:
/// each names its mode and its terminal place as its `origin`, and a last
/// session its execution, which a workspace's single unnamed one, from
/// before workspaces kept one per agent, takes as a new name.
pub(super) fn saved_agent_files(db: &Connection) -> Result<()> {
    let Some(state) = super::state_directory(db) else {
        return Ok(());
    };
    let spaces = state.join("workspaces");
    let Ok(entries) = fs::read_dir(&spaces) else {
        return Ok(());
    };
    for space in entries {
        let space = space?.path();
        if space.is_dir() {
            name_unnamed_last_session(&space)?;
            for file in fs::read_dir(&space)? {
                current_saved_agent(&file?.path())?;
            }
        }
    }
    Ok(())
}

/// What a workspace kept its one last session in before agents kept one
/// each.
const UNNAMED_LAST_SESSION: &str = "last-session.json";
const LAST_SESSION_SUFFIX: &str = ".last-session.json";
const RECORD_SUFFIX: &str = ".recovery.json";

/// Give the unnamed last session in `space` a file named after a new
/// execution of its own.
fn name_unnamed_last_session(space: &Path) -> Result<()> {
    let unnamed = space.join(UNNAMED_LAST_SESSION);
    if !unnamed.is_file() {
        return Ok(());
    }
    let execution = shoal_ids::execution_ids::ExecutionId::generate();
    let named = space.join(format!("{execution}{LAST_SESSION_SUFFIX}"));
    let Some(mut last) = saved_object(&unnamed)? else {
        return Ok(());
    };
    last.insert("execution".into(), json!(execution));
    write_saved(&named, &last)?;
    fs::remove_file(&unnamed)?;
    Ok(())
}

/// Rewrite the stopped agent's record or the last session at `path` into
/// the current shape; other files stay as they are.
fn current_saved_agent(path: &Path) -> Result<()> {
    let Some(name) = path.file_name().and_then(|name| name.to_str()) else {
        return Ok(());
    };
    let execution = match name.strip_suffix(LAST_SESSION_SUFFIX) {
        Some(execution) => Some(execution.to_owned()),
        None if name.ends_with(RECORD_SUFFIX) => None,
        None => return Ok(()),
    };
    let Some(mut saved) = saved_object(path)? else {
        return Ok(());
    };
    let before = saved.clone();
    origin_from_place(&mut saved);
    filled(&mut saved, "mode", default_mode);
    if let Some(execution) = execution {
        filled(&mut saved, "execution", || json!(execution));
    }
    if saved != before {
        write_saved(path, &saved)?;
    }
    Ok(())
}

/// The object the file at `path` holds; `None` for one that holds none,
/// such as a truncated write, which its readers skip as this leaves it.
fn saved_object(path: &Path) -> Result<Option<Map<String, Value>>> {
    Ok(match serde_json::from_slice(&fs::read(path)?) {
        Ok(Value::Object(saved)) => Some(saved),
        _ => None,
    })
}

/// Replace the file at `path` with `saved`, private as Shoal writes it.
fn write_saved(path: &Path, saved: &Map<String, Value>) -> Result<()> {
    let options = ReplaceOptions {
        permissions: Permissions::Temporary,
        sync: true,
    };
    replace_atomically(path, &serde_json::to_vec(saved)?, options)?;
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

/// Apply `change` to each stopped agent's record and each last session of
/// a removal record.
fn for_each_agent(removed: &mut Map<String, Value>, change: impl Fn(&mut Map<String, Value>)) {
    for agent in objects(removed, "agents") {
        if let Some(record) = agent.get_mut("record").and_then(Value::as_object_mut) {
            change(record);
        }
    }
    for last in objects(removed, "last_sessions") {
        change(last);
    }
}

/// Move `record`'s single value `one` into the list `many`, unless it has
/// that list already.
fn listed(record: &mut Map<String, Value>, one: &str, many: &str) {
    if let Some(value) = record.remove(one).filter(|value| !value.is_null()) {
        (record.entry(many)).or_insert_with(|| Value::Array(vec![value]));
    }
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
