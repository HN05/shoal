//! The preparation of migration 302. It rewrites the comments and reviews
//! that GitHub activity records hold from the shape GitHub's GraphQL API
//! gave them into the one its REST reads give: keyed by database ID, with
//! the fingerprint the REST reads record.
use anyhow::Result;
use base64::{
    Engine as _,
    engine::general_purpose::{STANDARD, URL_SAFE_NO_PAD},
};
use rusqlite::{Connection, params};
use serde_json::{Map, Value, json};

/// The tables that hold what a space's watch last saw of an item.
const ACTIVITY_TABLES: &[&str] = &["pr_activity", "space_item_activity"];

pub(super) fn key_by_database_ids(db: &Connection) -> Result<()> {
    for table in ACTIVITY_TABLES {
        let rows = {
            let mut reading = db.prepare(&format!("SELECT rowid,record FROM {table}"))?;
            reading
                .query_map([], |row| {
                    Ok((row.get::<_, i64>(0)?, row.get::<_, String>(1)?))
                })?
                .collect::<rusqlite::Result<Vec<_>>>()?
        };
        for (rowid, text) in rows {
            let Ok(mut record) = serde_json::from_str::<Value>(&text) else {
                continue;
            };
            if rewritten(&mut record) {
                db.execute(
                    &format!("UPDATE {table} SET record=?2 WHERE rowid=?1"),
                    params![rowid, serde_json::to_string(&record)?],
                )?;
            }
        }
    }
    Ok(())
}

/// Rewrite the GitHub comments and reviews `record` holds, and the keys of
/// those the space posted; whether any were.
fn rewritten(record: &mut Value) -> bool {
    let comments = (record.get_mut("comments"))
        .and_then(Value::as_object_mut)
        .is_some_and(rewrite_comments);
    let authored = (record.get_mut("authored"))
        .and_then(Value::as_array_mut)
        .is_some_and(|authored| rewrite_authored(authored));
    comments || authored
}

/// Put each GitHub entry of `comments` in the REST shape; whether any was.
fn rewrite_comments(comments: &mut Map<String, Value>) -> bool {
    let old: Vec<String> = (comments.iter())
        .filter(|(_, entry)| graphql_fingerprint(entry).is_some())
        .map(|(key, _)| key.clone())
        .collect();
    for key in &old {
        if let Some(entry) = comments.remove(key)
            && let Some((key, entry)) = rest_entry(key, entry)
        {
            comments.insert(key, entry);
        }
    }
    !old.is_empty()
}

/// Name each comment of `authored` that names a node ID by its database
/// ID; whether any did.
fn rewrite_authored(authored: &mut [Value]) -> bool {
    let mut changed = false;
    for key in authored {
        if let Some(rest) = key.as_str().and_then(rest_key) {
            *key = rest.into();
            changed = true;
        }
    }
    changed
}

/// The fingerprint `entry` holds when GitHub's GraphQL shape wrote it,
/// which no other forge's has: it names `lastEditedAt`.
fn graphql_fingerprint(entry: &Value) -> Option<Value> {
    let fingerprint = entry["fingerprint"].as_str()?;
    serde_json::from_str::<Value>(fingerprint)
        .ok()
        .filter(|fingerprint| fingerprint.get("lastEditedAt").is_some())
}

/// The entry under `key` in the REST shape, under its new key. An ID that
/// decodes to none, which GitHub never gave, drops the entry.
fn rest_entry(key: &str, mut entry: Value) -> Option<(String, Value)> {
    let old = graphql_fingerprint(&entry)?;
    let key = match rest_key(key) {
        Some(key) => key,
        None if numeric(key) => key.to_owned(),
        None => return None,
    };
    let fingerprint = match key.split_once(':') {
        Some(("reviews", _)) => json!({"body": old["body"], "state": old["state"]}),
        _ => json!({"body": old["body"]}),
    };
    entry["fingerprint"] = fingerprint.to_string().into();
    Some((key, entry))
}

/// Whether `key` already names its comment by a database ID.
fn numeric(key: &str) -> bool {
    key.split_once(':')
        .is_some_and(|(_, id)| id.parse::<u64>().is_ok())
}

/// `key` with the node ID it names replaced by its database ID; none when
/// it names no node ID.
fn rest_key(key: &str) -> Option<String> {
    let (source, id) = key.split_once(':')?;
    if id.parse::<u64>().is_ok() {
        return None;
    }
    Some(format!("{source}:{}", database_id(id)?))
}

/// The database ID GitHub's node ID `node` encodes: in the current format
/// a type prefix and the URL-safe base64 of a MessagePack array that ends
/// with it, as in `IC_kwDO…`; in the legacy one the base64 of the type
/// name's length, a colon, the type name and the ID, as in
/// `012:IssueComment1`.
fn database_id(node: &str) -> Option<u64> {
    match node.split_once('_') {
        Some((_, packed)) => last_packed_integer(&URL_SAFE_NO_PAD.decode(packed).ok()?),
        None => {
            let legacy = String::from_utf8(STANDARD.decode(node).ok()?).ok()?;
            let (length, rest) = legacy.split_once(':')?;
            rest.get(length.parse::<usize>().ok()?..)?.parse().ok()
        }
    }
}

/// The last element of the MessagePack array of unsigned integers `bytes`.
fn last_packed_integer(bytes: &[u8]) -> Option<u64> {
    let (&header, mut rest) = bytes.split_first()?;
    let count = match header {
        0x91..=0x9f => header - 0x90,
        _ => return None,
    };
    let mut last = None;
    for _ in 0..count {
        let (&tag, tail) = rest.split_first()?;
        let width = match tag {
            0x00..=0x7f => 0,
            0xcc => 1,
            0xcd => 2,
            0xce => 4,
            0xcf => 8,
            _ => return None,
        };
        let (digits, tail) = tail.split_at_checked(width)?;
        last = Some(match width {
            0 => u64::from(tag),
            _ => digits.iter().fold(0, |id, &byte| id << 8 | u64::from(byte)),
        });
        rest = tail;
    }
    last
}
