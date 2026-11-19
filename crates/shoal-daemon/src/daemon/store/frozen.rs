//! Copies of the code the migrations once called elsewhere, frozen at the
//! shapes they migrate, so a later change to that code never changes what a
//! migration does.
use std::collections::BTreeMap;

use anyhow::{Context, Result, ensure};
use rusqlite::Connection;
use serde::Deserialize;
use shoal_ids::space_ids::SpaceNumber;

mod directory_identity;
pub(super) use directory_identity::birth_identity;
#[cfg(test)]
pub(super) use directory_identity::directory_identity;

/// The longest space name, a cut name included.
const MAX_SPACE_NAME_LEN: usize = 40;

/// The longest directory name.
const MAX_NAME_LEN: usize = 64;

/// The mode a record without one ran in.
pub(super) const DEFAULT_MODE: &str = "worker";

/// A question's or approval request's number within its space, which reads
/// `<space>.<item>`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(super) struct ItemNumber {
    pub space: SpaceNumber,
    pub item: u32,
}

impl std::fmt::Display for ItemNumber {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}.{}", self.space, self.item)
    }
}

/// The smallest item number no question or approval request of the space
/// numbered `space` holds.
pub(super) fn free_item(db: &Connection, space: SpaceNumber) -> Result<u32> {
    Ok(db.query_row(
        "WITH held(number) AS (
            SELECT number FROM questions WHERE space_number=?1
            UNION ALL SELECT number FROM access_requests WHERE space_number=?1)
        SELECT MIN(n) FROM (SELECT 1 AS n UNION ALL SELECT number+1 FROM held)
            WHERE n NOT IN (SELECT number FROM held)",
        [space],
        |row| row.get(0),
    )?)
}

/// The name Shoal gives a space from `text`, such as an issue's title:
/// control characters and bidirectional overrides become spaces, whitespace
/// collapses, and a name over the limit is cut to end with `…`; `fallback`
/// when nothing is left.
pub(super) fn space_name(text: &str, fallback: &str) -> String {
    let cleaned: String = (text.chars())
        .map(|c| if forbidden(c) { ' ' } else { c })
        .collect();
    let name = match collapsed(&cleaned) {
        name if name.is_empty() => fallback.to_owned(),
        name => name,
    };
    if name.chars().count() <= MAX_SPACE_NAME_LEN {
        return name;
    }
    let kept: String = name.chars().take(MAX_SPACE_NAME_LEN - 1).collect();
    format!("{}…", kept.trim_end())
}

fn forbidden(c: char) -> bool {
    c.is_control() || matches!(c, '\u{202A}'..='\u{202E}' | '\u{2066}'..='\u{2069}')
}

fn collapsed(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// The name of a directory made from `text`: ASCII letters, digits, `-` and
/// `_`, with every other character `-`, never starting with `-` or `_`, cut
/// to the longest name; `workspace` when nothing is left.
pub(super) fn directory_name(text: &str) -> String {
    let mapped: String = text
        .chars()
        .map(
            |c| match c.is_ascii_alphanumeric() || matches!(c, '-' | '_') {
                true => c,
                false => '-',
            },
        )
        .collect();
    match mapped.trim_start_matches(['-', '_']) {
        "" => "workspace".to_owned(),
        name => name.chars().take(MAX_NAME_LEN).collect(),
    }
}

/// `name`, or with `-<suffix>` from 2 on, cut to fit the longest name.
pub(super) fn suffixed_directory(name: &str, suffix: u64) -> String {
    let suffix = match suffix {
        1 => String::new(),
        n => format!("-{n}"),
    };
    let kept = MAX_NAME_LEN.saturating_sub(suffix.len());
    let mut name: String = name.chars().take(kept).collect();
    name.push_str(&suffix);
    name
}

/// The repository name a clone URL or path ends with, without `.git`.
pub(super) fn source_name(source: &str) -> &str {
    let source = source.trim_end_matches('/');
    let last = source.rsplit(['/', ':']).next().unwrap_or(source);
    let last = last.trim_end_matches('/');
    last.strip_suffix(".git").unwrap_or(last)
}

/// A place in a terminal host, as a recorded origin names it.
#[derive(Deserialize)]
pub(super) struct Place {
    pub host: String,
    #[serde(default)]
    pub place: Option<String>,
    #[serde(default)]
    pub data: BTreeMap<String, String>,
    #[serde(default)]
    pub instance: Option<String>,
}

impl Place {
    /// What names the server holding the place among its host's servers;
    /// empty for a host with one server.
    pub(super) fn server(&self) -> String {
        match self.host.as_str() {
            "herdr" => ["HERDR_SOCKET_PATH", "HERDR_SESSION"]
                .iter()
                .find_map(|name| Some(format!("{name}={}", self.data.get(*name)?)))
                .unwrap_or_default(),
            "rex" => self.data.get("server").cloned().unwrap_or_default(),
            _ => String::new(),
        }
    }
}

/// The ownership a simulator record names, which the index of its owner
/// reads.
#[derive(Deserialize)]
#[allow(dead_code, reason = "decoding checks the fields' types")]
struct SimulatorOwner {
    workspace_id: Option<String>,
    last_workspace_id: Option<String>,
}

/// Refuse simulator records whose ownership the owner index cannot read:
/// each is a JSON object naming its owners as strings.
pub(super) fn validate_simulator_records(db: &Connection) -> Result<()> {
    let records = db
        .prepare("SELECT id,record FROM simulators ORDER BY id")?
        .query_map([], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    for (id, record) in records {
        // Serde also accepts struct-shaped arrays; SQL ownership paths do not.
        ensure!(
            record.trim_start().starts_with('{'),
            "decode simulator record {id}: expected a JSON object"
        );
        serde_json::from_str::<SimulatorOwner>(&record)
            .with_context(|| format!("decode simulator record {id}"))
            .context("validate simulator records before indexing ownership")?;
    }
    Ok(())
}
