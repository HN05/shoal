//! The preparation of migration 301, which rewrites the choices thread
//! posts offered into the surfaces `shoal send --choice` builds, and each
//! pick of one, by a post or by resolving the thread, into an answer to it,
//! in the thread tables and in the requests and summaries the hub keeps.
use anyhow::Result;
use rusqlite::{Connection, params};
use serde_json::{Map, Value, json};
use shoal_protocol::{surfaces::Surface, threads::Answer};

pub(super) fn rewrite_choices(db: &Connection) -> Result<()> {
    offer_surfaces(db)?;
    answer_picks(db)?;
    answer_resolved_picks(db)?;
    rewrite_hub_payloads(db)
}

/// Give each post that offered choices, or a recommendation or reason
/// without them, the surface that shows them.
fn offer_surfaces(db: &Connection) -> Result<()> {
    let mut offering = db.prepare(
        "SELECT id,choices,recommendation,reason FROM thread_posts
            WHERE choices!='[]' OR recommendation IS NOT NULL OR reason IS NOT NULL",
    )?;
    let offers = offering.query_map([], |row| {
        let choices: String = row.get(1)?;
        Ok((
            row.get::<_, i64>(0)?,
            choices,
            row.get::<_, Option<String>>(2)?,
            row.get::<_, Option<String>>(3)?,
        ))
    })?;
    for offer in offers {
        let (id, choices, recommendation, reason) = offer?;
        let choices: Vec<String> = serde_json::from_str(&choices)?;
        let surface = Surface::choices(&choices, recommendation.as_deref(), reason.as_deref());
        db.execute(
            "UPDATE thread_posts SET ui=?2 WHERE id=?1",
            params![id, serde_json::to_string(surface.as_json())?],
        )?;
    }
    Ok(())
}

/// Make each post that picked a choice the answer to the latest earlier
/// post from the other side that offered choices, as the pick was checked
/// against; a pick with no such post stays a plain post.
fn answer_picks(db: &Connection) -> Result<()> {
    let mut picking = db.prepare(
        "SELECT p.id,p.choice,(SELECT o.id FROM thread_posts o
                WHERE o.thread_id=p.thread_id AND o.id<p.id AND o.choices!='[]'
                    AND o.side IS NOT NULL AND o.side IS NOT p.side
                ORDER BY o.id DESC LIMIT 1)
            FROM thread_posts p WHERE p.choice IS NOT NULL AND p.side IS NOT NULL",
    )?;
    let picks = picking.query_map([], |row| {
        Ok((
            row.get::<_, i64>(0)?,
            row.get::<_, i64>(1)?,
            row.get::<_, Option<i64>>(2)?,
        ))
    })?;
    for pick in picks {
        let (id, choice, Some(offer)) = pick? else {
            continue;
        };
        db.execute(
            "UPDATE thread_posts SET answer=?2 WHERE id=?1",
            params![id, answer(offer, choice)?],
        )?;
    }
    Ok(())
}

/// Give each thread resolved as answered with a choice a post that picks
/// it, from whoever resolved it, on their side, when it was resolved: an
/// answer to the latest post that offered choices, from either side, as
/// the pick was checked against, with the choice's text. The resolver is the holder when the
/// holder resolved it, the user holding it or its holding space, and else
/// the opener. The space's journal, which recorded the resolution then,
/// records neither the post nor the outcome without the choice.
fn answer_resolved_picks(db: &Connection) -> Result<()> {
    let mut resolving = db.prepare(
        "WITH resolved AS (SELECT id,resolved_by,resolver_id,resolved_at,
                json_extract(outcome,'$.choice') AS choice,
                CASE WHEN (resolved_by='user' AND holder_id IS NULL)
                        OR (resolved_by='space' AND resolver_id IS holder_id)
                    THEN 'holder' ELSE 'opener' END AS side
                FROM threads WHERE json_extract(outcome,'$.choice') IS NOT NULL)
        SELECT r.id,r.resolved_by,r.resolver_id,r.resolved_at,r.choice,r.side,o.id,o.choices
            FROM resolved r JOIN thread_posts o ON o.id=(SELECT id FROM thread_posts
                WHERE thread_id=r.id AND choices!='[]' ORDER BY id DESC LIMIT 1)",
    )?;
    let resolved = resolving
        .query_map([], |row| {
            Ok(ResolvedPick {
                thread: row.get(0)?,
                by: row.get(1)?,
                resolver: row.get(2)?,
                at: row.get(3)?,
                choice: row.get(4)?,
                side: row.get(5)?,
                offer: row.get(6)?,
                choices: row.get(7)?,
            })
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    unjournaled(db, &["thread_posted", "thread_resolved"], || {
        for pick in resolved {
            pick.record(db)?;
        }
        db.execute(
            "UPDATE threads SET outcome=json_remove(outcome,'$.choice')
                WHERE json_extract(outcome,'$.choice') IS NOT NULL",
            [],
        )?;
        Ok(())
    })
}

/// A thread resolved as answered with the choice `choice` of the post
/// `offer`, from its side `side`.
struct ResolvedPick {
    thread: i64,
    by: String,
    resolver: Option<String>,
    at: i64,
    choice: i64,
    side: String,
    offer: i64,
    choices: String,
}

impl ResolvedPick {
    fn record(&self, db: &Connection) -> Result<()> {
        let choices: Vec<String> = serde_json::from_str(&self.choices)?;
        let text = (usize::try_from(self.choice - 1).ok())
            .and_then(|index| choices.get(index))
            .cloned()
            .unwrap_or_else(|| self.choice.to_string());
        db.execute(
            "INSERT INTO thread_posts(thread_id,created_at,side,sender,sender_id,text,answer)
                VALUES (?1,?2,?3,?4,?5,?6,?7)",
            params![
                self.thread,
                self.at,
                self.side,
                self.by,
                self.resolver,
                text,
                answer(self.offer, self.choice)?
            ],
        )?;
        Ok(())
    }
}

/// The answer that picks the choice `choice` of the post `offer`, as JSON.
fn answer(offer: i64, choice: i64) -> Result<String> {
    let answer = Answer {
        post: Some(offer),
        action: choice.to_string(),
        values: serde_json::Map::new(),
    };
    Ok(serde_json::to_string(&answer)?)
}

/// Run `change` without the triggers `names`, which would journal what it
/// rewrites as if it happened now.
fn unjournaled(db: &Connection, names: &[&str], change: impl FnOnce() -> Result<()>) -> Result<()> {
    let mut triggers = Vec::new();
    for name in names {
        let sql: String = db.query_row(
            "SELECT sql FROM sqlite_schema WHERE type='trigger' AND name=?1",
            [name],
            |row| row.get(0),
        )?;
        db.execute_batch(&format!("DROP TRIGGER {name}"))?;
        triggers.push(sql);
    }
    change()?;
    for sql in triggers {
        db.execute_batch(&sql)?;
    }
    Ok(())
}

/// The hub's columns that hold requests or space summaries as JSON, which
/// may carry posts, threads and resolutions in their old shape.
const HUB_PAYLOADS: &[(&str, &str)] = &[
    ("hub_queue", "call"),
    ("hub_outbox", "call"),
    ("hub_spaces", "summary"),
    ("hub_space_copies", "summary"),
];

/// Rewrite the posts, threads and resolutions the hub's requests and space
/// summaries hold, as the thread tables are rewritten.
fn rewrite_hub_payloads(db: &Connection) -> Result<()> {
    for (table, column) in HUB_PAYLOADS {
        let rows = {
            let mut reading = db.prepare(&format!("SELECT rowid,{column} FROM {table}"))?;
            reading
                .query_map([], |row| {
                    Ok((row.get::<_, i64>(0)?, row.get::<_, String>(1)?))
                })?
                .collect::<rusqlite::Result<Vec<_>>>()?
        };
        for (rowid, text) in rows {
            let Ok(mut payload) = serde_json::from_str::<Value>(&text) else {
                continue;
            };
            if rewritten(&mut payload) {
                db.execute(
                    &format!("UPDATE {table} SET {column}=?2 WHERE rowid=?1"),
                    params![rowid, serde_json::to_string(&payload)?],
                )?;
            }
        }
    }
    Ok(())
}

/// The fields an old post's choices took, beside its text.
const OLD_POST_FIELDS: &[&str] = &["choices", "recommendation", "reason", "choice"];

/// Rewrite every old post, thread and resolution in `value`; whether any
/// was.
fn rewritten(value: &mut Value) -> bool {
    let mut changed = false;
    match value {
        Value::Array(items) => {
            for item in items {
                changed |= rewritten(item);
            }
        }
        Value::Object(fields) => {
            if let Some(Value::Array(posts)) = fields.get_mut("posts") {
                changed |= answer_listed_picks(posts);
            }
            changed |= offer_surface(fields);
            changed |= drop_resolution_choice(fields);
            for value in fields.values_mut() {
                changed |= rewritten(value);
            }
        }
        _ => {}
    }
    changed
}

/// Make each post of a thread's `posts` that picked a choice the answer to
/// the latest earlier post from the other side that offered choices, as
/// the thread tables' picks become; one with no such post picks nothing.
fn answer_listed_picks(posts: &mut [Value]) -> bool {
    let mut changed = false;
    let mut offers: Vec<(Value, i64)> = Vec::new();
    for post in posts.iter_mut().filter_map(Value::as_object_mut) {
        let side = post.get("side").cloned().unwrap_or(Value::Null);
        if let Some(choice) = post.remove("choice") {
            changed = true;
            let offer = (offers.iter().rev())
                .find(|(offered, _)| !side.is_null() && *offered != side)
                .map(|(_, id)| *id);
            if let (Some(offer), Some(choice)) = (offer, choice.as_i64()) {
                post.insert(
                    "answer".into(),
                    json!({"post": offer, "action": choice.to_string()}),
                );
            }
        }
        let offered = post
            .get("choices")
            .and_then(Value::as_array)
            .is_some_and(|c| !c.is_empty());
        if let (true, Some(id)) = (offered, post.get("id").and_then(Value::as_i64)) {
            offers.push((side, id));
        }
    }
    changed
}

/// Give a post in its old shape, its text with any of [`OLD_POST_FIELDS`],
/// the surface that shows its choices and the answer its pick makes to the
/// latest surface the other side posted.
fn offer_surface(fields: &mut Map<String, Value>) -> bool {
    if !fields.contains_key("text") || !OLD_POST_FIELDS.iter().any(|key| fields.contains_key(*key))
    {
        return false;
    }
    let choices: Vec<String> = (fields.remove("choices"))
        .and_then(|choices| serde_json::from_value(choices).ok())
        .unwrap_or_default();
    let text = |value: Option<Value>| value.and_then(|value| value.as_str().map(str::to_owned));
    let recommendation = text(fields.remove("recommendation"));
    let reason = text(fields.remove("reason"));
    if !choices.is_empty() || recommendation.is_some() || reason.is_some() {
        let surface = Surface::choices(&choices, recommendation.as_deref(), reason.as_deref());
        fields.insert("ui".into(), surface.as_json().clone());
    }
    if let Some(choice) = fields.remove("choice").as_ref().and_then(Value::as_i64) {
        fields.insert("answer".into(), json!({"action": choice.to_string()}));
    }
    true
}

/// Drop the choice of a resolution as answered, as a thread's outcome or
/// a request to resolve one names it, which the thread tables keep as an
/// answer post instead; a summary's or request's resolution has no posts
/// to keep it in.
fn drop_resolution_choice(fields: &mut Map<String, Value>) -> bool {
    fields.get("outcome").and_then(Value::as_str) == Some("answered")
        && fields.remove("choice").is_some()
}
