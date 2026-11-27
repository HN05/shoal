//! What the runs Shoal measures used, kept by repository, kind and key, and
//! the memory a run of a key is expected to use, which admission counts it
//! at. docs/design/queue.md says why the estimate reads what it does.
use std::time::Duration;

use anyhow::Result;
use rusqlite::{Connection, params};
use shoal_ids::repository_ids::RepositoryId;
use shoal_protocol::runs::CommandKey;

use crate::time::UnixTime;

/// How many of a key's latest whole runs its estimate reads, whatever
/// their age.
const WHOLE_RUNS: i64 = 30;

/// The share of a key's whole runs whose peaks its estimate covers.
const PERCENTILE: f64 = 0.95;

/// How much more than its peak a run the kernel's OOM killer hit counts
/// at, since it needed more than it got.
const OOM_KILLED_FACTOR: f64 = 1.2;

/// How long a key's runs are kept after its latest one ended.
const KEPT_FOR: Duration = Duration::from_secs(30 * 24 * 60 * 60);

/// What tells runs apart whose peaks one estimate reads.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RunKey {
    /// A CI job, by its workflow and job name.
    CiJob { workflow: String, name: String },
    /// A `shoal run` command.
    Command(CommandKey),
    /// The repository's seed build.
    SeedBuild,
}

impl RunKey {
    /// The kind the runs table records it as.
    fn kind(&self) -> &'static str {
        match self {
            Self::CiJob { .. } => "ci_job",
            Self::Command(_) => "command",
            Self::SeedBuild => "seed_build",
        }
    }

    /// The text the runs table records it as, within its kind.
    fn text(&self) -> String {
        match self {
            Self::CiJob { workflow, name } => serde_json::json!([workflow, name]).to_string(),
            Self::Command(key) => serde_json::to_string(key).expect("a command key serializes"),
            Self::SeedBuild => serde_json::json!("seed").to_string(),
        }
    }
}

/// What a run used.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct RunUsage {
    pub ended_at: UnixTime,
    /// The most memory it held, what it had swapped out included.
    pub peak_memory: u64,
    pub duration: Duration,
    /// Whether it ran to its end, rather than being cut short, as when load
    /// protection or the user stopped it.
    pub whole: bool,
    /// Whether the kernel's OOM killer killed a process of it.
    pub oom_killed: bool,
}

/// Keep `run` as the latest run of `key` in `repository`, forgetting the
/// runs no estimate reads any more.
pub fn record_run(
    db: &Connection,
    repository: &RepositoryId,
    key: &RunKey,
    run: &RunUsage,
) -> Result<()> {
    db.execute(
        "INSERT INTO run_usage(repository_id,kind,key,ended_at,peak_memory,duration,whole,oom_killed)
         VALUES (?1,?2,?3,?4,?5,?6,?7,?8)",
        params![
            repository,
            key.kind(),
            key.text(),
            run.ended_at,
            i64::try_from(run.peak_memory).unwrap_or(i64::MAX),
            i64::try_from(run.duration.as_secs()).unwrap_or(i64::MAX),
            run.whole,
            run.oom_killed,
        ],
    )?;
    forget_unread_runs(db, repository, key)?;
    forget_idle_keys(db, run.ended_at - KEPT_FOR)
}

/// What a run of `key` in `repository` is expected to use: the
/// [`PERCENTILE`] of the peaks of its latest [`WHOLE_RUNS`] whole runs, or
/// while it has none the highest peak of a run cut short, none before it
/// has run.
pub fn expected_memory(
    db: &Connection,
    repository: &RepositoryId,
    key: &RunKey,
) -> Result<Option<u64>> {
    let peaks = whole_peaks(db, repository, key)?;
    if peaks.is_empty() {
        return highest_peak(db, repository, key);
    }
    Ok(Some(percentile(peaks) as u64))
}

/// The median duration of a key's runs, which the hub compares machines'
/// speeds by.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MedianDuration {
    pub repository: RepositoryId,
    /// The key's kind and text as the runs table records them, which tell
    /// it from other keys on every machine.
    pub run: String,
    pub seconds: u64,
}

/// The median duration of each key's whole runs that ended from `since`,
/// leaving out those the OOM killer hit, which ran short of memory rather
/// than at the machine's speed.
pub fn median_durations(db: &Connection, since: UnixTime) -> Result<Vec<MedianDuration>> {
    let medians = (durations_by_key(db, since)?.into_iter())
        .map(|((repository, run), durations)| MedianDuration {
            repository,
            run,
            seconds: median(&durations),
        })
        .collect();
    Ok(medians)
}

/// Which repository a key's runs are of, and the key as a
/// [`MedianDuration`] names it.
type Keyed = (RepositoryId, String);

/// The durations of the runs [`median_durations`] reads, by key, each
/// key's in order.
fn durations_by_key(db: &Connection, since: UnixTime) -> Result<Vec<(Keyed, Vec<u64>)>> {
    let mut statement = db.prepare(
        "SELECT repository_id, kind, key, duration FROM run_usage
         WHERE whole AND NOT oom_killed AND duration IS NOT NULL AND ended_at>=?1
         ORDER BY repository_id, kind, key, duration",
    )?;
    let rows = statement.query_map([since], |row| {
        let (kind, key): (String, String) = (row.get(1)?, row.get(2)?);
        let seconds: i64 = row.get(3)?;
        Ok(((row.get(0)?, format!("{kind} {key}")), seconds))
    })?;
    let mut keys: Vec<(Keyed, Vec<u64>)> = Vec::new();
    for row in rows {
        let (key, seconds) = row?;
        let seconds = u64::try_from(seconds).unwrap_or_default();
        match keys.last_mut() {
            Some((last, durations)) if *last == key => durations.push(seconds),
            _ => keys.push((key, vec![seconds])),
        }
    }
    Ok(keys)
}

/// The median of `sorted`, which holds at least one duration.
fn median(sorted: &[u64]) -> u64 {
    let middle = sorted.len() / 2;
    match sorted.len() % 2 {
        1 => sorted[middle],
        _ => (sorted[middle - 1] + sorted[middle]) / 2,
    }
}

/// The condition that selects the runs of the key of repository `?1`,
/// kind `?2` and text `?3`.
const KEY: &str = "repository_id=?1 AND kind=?2 AND key=?3";

/// The condition that selects the runs an estimate reads as whole: one the
/// OOM killer hit needed at least what it held, however it ended.
const WHOLE: &str = "(whole OR oom_killed)";

/// The peaks of the latest [`WHOLE_RUNS`] whole runs of `key`, each one the
/// OOM killer hit raised by [`OOM_KILLED_FACTOR`].
fn whole_peaks(db: &Connection, repository: &RepositoryId, key: &RunKey) -> Result<Vec<f64>> {
    let mut statement = db.prepare(&format!(
        "SELECT peak_memory, oom_killed FROM run_usage WHERE {KEY} AND {WHOLE}
         ORDER BY ended_at DESC, rowid DESC LIMIT ?4"
    ))?;
    let rows = statement.query_map(
        params![repository, key.kind(), key.text(), WHOLE_RUNS],
        |row| {
            let (peak, oom_killed): (i64, bool) = (row.get(0)?, row.get(1)?);
            Ok(peak as f64 * if oom_killed { OOM_KILLED_FACTOR } else { 1.0 })
        },
    )?;
    Ok(rows.collect::<rusqlite::Result<_>>()?)
}

/// The highest peak among the runs of `key`.
fn highest_peak(db: &Connection, repository: &RepositoryId, key: &RunKey) -> Result<Option<u64>> {
    let peak: Option<i64> = db.query_row(
        &format!("SELECT max(peak_memory) FROM run_usage WHERE {KEY}"),
        params![repository, key.kind(), key.text()],
        |row| row.get(0),
    )?;
    Ok(peak.and_then(|peak| u64::try_from(peak).ok()))
}

/// The [`PERCENTILE`] of `peaks`, at least one, interpolated between the
/// two nearest.
fn percentile(mut peaks: Vec<f64>) -> f64 {
    peaks.sort_by(f64::total_cmp);
    let rank = PERCENTILE * (peaks.len() - 1) as f64;
    let below = rank.floor() as usize;
    let above = (below + 1).min(peaks.len() - 1);
    peaks[below] + (peaks[above] - peaks[below]) * (rank - below as f64)
}

/// Forget the whole runs of `key` before its latest [`WHOLE_RUNS`], and its
/// runs cut short once it has a whole one.
fn forget_unread_runs(db: &Connection, repository: &RepositoryId, key: &RunKey) -> Result<()> {
    db.execute(
        &format!(
            "DELETE FROM run_usage WHERE {KEY}
             AND rowid NOT IN (SELECT rowid FROM run_usage WHERE {KEY} AND {WHOLE}
                 ORDER BY ended_at DESC, rowid DESC LIMIT ?4)
             AND ({WHOLE} OR EXISTS (SELECT 1 FROM run_usage WHERE {KEY} AND {WHOLE}))"
        ),
        params![repository, key.kind(), key.text(), WHOLE_RUNS],
    )?;
    Ok(())
}

/// Forget every key whose latest run ended before `before`.
fn forget_idle_keys(db: &Connection, before: UnixTime) -> Result<()> {
    db.execute(
        "DELETE FROM run_usage WHERE (repository_id,kind,key) IN (
             SELECT repository_id,kind,key FROM run_usage
             GROUP BY repository_id,kind,key HAVING max(ended_at)<?1)",
        [before],
    )?;
    Ok(())
}

#[cfg(test)]
mod tests;
