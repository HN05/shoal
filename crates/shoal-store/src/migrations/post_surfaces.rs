//! The preparation of migration 301, which rewrites the choices thread
//! posts offered into the surfaces `shoal send --choice` builds, and each
//! pick of one, by a post or by resolving the thread, into an answer to it.
use anyhow::Result;
use rusqlite::{Connection, params};
use shoal_protocol::{surfaces::Surface, threads::Answer};

pub(super) fn rewrite_choices(db: &Connection) -> Result<()> {
    offer_surfaces(db)?;
    answer_picks(db)?;
    answer_resolved_picks(db)
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
/// it, from its opener, who resolved it, when it was resolved: an answer to
/// the latest post that offered choices, with the choice's text. The
/// space's journal, which recorded the resolution then, records neither
/// the post nor the outcome without the choice.
fn answer_resolved_picks(db: &Connection) -> Result<()> {
    let mut resolving = db.prepare(
        "SELECT t.id,t.resolved_by,t.resolver_id,t.resolved_at,
                json_extract(t.outcome,'$.choice'),o.id,o.choices
            FROM threads t JOIN thread_posts o ON o.id=(SELECT id FROM thread_posts
                WHERE thread_id=t.id AND choices!='[]' ORDER BY id DESC LIMIT 1)
            WHERE json_extract(t.outcome,'$.choice') IS NOT NULL",
    )?;
    let resolved = resolving
        .query_map([], |row| {
            Ok(ResolvedPick {
                thread: row.get(0)?,
                by: row.get(1)?,
                resolver: row.get(2)?,
                at: row.get(3)?,
                choice: row.get(4)?,
                offer: row.get(5)?,
                choices: row.get(6)?,
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
/// `offer`.
struct ResolvedPick {
    thread: i64,
    by: String,
    resolver: Option<String>,
    at: i64,
    choice: i64,
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
                VALUES (?1,?2,'opener',?3,?4,?5,?6)",
            params![
                self.thread,
                self.at,
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
