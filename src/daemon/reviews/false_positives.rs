//! A repository's false positives: claims its reviews raised that were
//! resolved as wrong, kept in a file beside its local config so that every
//! later review of the repository receives them and the user can edit them.
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use toml_edit::{ArrayOfTables, DocumentMut, Item, Table, Value, value};

use super::super::workspace::Manager;
use crate::{
    fsutil::{self, Permissions, ReplaceOptions},
    reviews::{FindingId, Outcome, Review},
};

/// What a new file starts with.
const HEADER: &str = "# Claims that Shoal's reviews of this repository raised and that were\n\
                      # resolved as wrong. Later reviews receive them; delete an entry to stop that.\n";

/// One claim resolved as wrong.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct FalsePositive {
    /// The finding it was resolved from; none for one the user added.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) finding: Option<FindingId>,
    pub(crate) claim: String,
    pub(crate) path: String,
    pub(crate) reason: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Entries {
    #[serde(default)]
    false_positive: Vec<FalsePositive>,
}

/// The context of an error reading a repository's false positives, which
/// fails the review as [`crate::reviews::FailureReason::InvalidFalsePositives`].
#[derive(Debug)]
pub(super) struct Unreadable;

impl std::fmt::Display for Unreadable {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("read the repository's false positives")
    }
}

impl Manager {
    /// The false positives of the repository `repository_id`, newest first.
    /// An error reading them has the context [`Unreadable`].
    pub(super) async fn false_positives(&self, repository_id: &str) -> Result<Vec<FalsePositive>> {
        let Some(path) = self.false_positives_path(repository_id).await? else {
            return Ok(Vec::new());
        };
        let listed = tokio::task::spawn_blocking(move || read(&path)).await?;
        listed.context(Unreadable)
    }

    /// Keep `finding` of `review` in its repository's false positives while
    /// `outcome` is one, or drop it from them, and return the file as it
    /// was when that changed it. The caller holds the registry gate, which
    /// moves and removal take, so the file stays where it was found.
    pub(super) async fn keep_false_positive(
        &self,
        review: &Review,
        finding: FindingId,
        outcome: Option<&Outcome>,
    ) -> Result<Option<Previous>> {
        let entry = match outcome {
            Some(Outcome::FalsePositive { reason }) => Some(entry(review, finding, reason)?),
            _ => None,
        };
        let repository_id = self
            .workspace_by_id(&review.workspace_id)
            .await?
            .repository_id;
        let path = match entry {
            Some(_) => Some(self.false_positives_path_reserved(&repository_id).await?),
            None => self.false_positives_path(&repository_id).await?,
        };
        let Some(path) = path else {
            return Ok(None);
        };
        tokio::task::spawn_blocking(move || record(&path, finding, entry)).await?
    }
}

/// A false positives file as it was before an edit, which [`Previous::restore`]
/// puts back.
#[derive(Debug)]
pub(super) struct Previous {
    path: PathBuf,
    /// None when the edit created the file.
    text: Option<String>,
}

impl Previous {
    pub(super) async fn restore(self) -> Result<()> {
        tokio::task::spawn_blocking(move || match &self.text {
            Some(text) => store(&self.path, text, true),
            None => std::fs::remove_file(&self.path)
                .with_context(|| format!("delete {}", self.path.display())),
        })
        .await?
    }
}

/// `finding` of `review` as a false positive for `reason`.
fn entry(review: &Review, finding: FindingId, reason: &str) -> Result<FalsePositive> {
    let resolved = review
        .findings
        .iter()
        .find(|listed| listed.id == finding)
        .with_context(|| format!("finding {finding} does not exist"))?;
    Ok(FalsePositive {
        finding: Some(finding),
        claim: resolved.title.clone(),
        path: resolved.path.clone(),
        reason: reason.to_owned(),
    })
}

/// The false positives in the file at `path`, newest first; none without
/// the file.
fn read(path: &Path) -> Result<Vec<FalsePositive>> {
    let Some(text) = load(path)? else {
        return Ok(Vec::new());
    };
    let mut entries = parse(&text).with_context(|| format!("parse {}", path.display()))?;
    entries.reverse();
    Ok(entries)
}

/// Replace the entry of `finding` in the file at `path` with `entry`, or
/// drop it without one, and return the file as it was when that changed it.
/// A missing file is created only for an entry.
fn record(
    path: &Path,
    finding: FindingId,
    entry: Option<FalsePositive>,
) -> Result<Option<Previous>> {
    let text = load(path)?;
    let edited = with_entry(text.as_deref(), finding, entry.as_ref())
        .with_context(|| format!("edit {}", path.display()))?;
    let Some(edited) = edited else {
        return Ok(None);
    };
    store(path, &edited, text.is_some())?;
    Ok(Some(Previous {
        path: path.to_owned(),
        text,
    }))
}

/// `text`, or a new file without it, with the entry of `finding` replaced
/// by `entry` or dropped without one, keeping its comments and layout;
/// none when nothing changes. Refuses an invalid result.
fn with_entry(
    text: Option<&str>,
    finding: FindingId,
    entry: Option<&FalsePositive>,
) -> Result<Option<String>> {
    let mut document: DocumentMut = text.unwrap_or(HEADER).parse()?;
    let removed = without_finding(&mut document, finding)?;
    if let Some(entry) = entry {
        append(&mut document, entry);
    }
    if !removed && entry.is_none() {
        return Ok(None);
    }
    let edited = document.to_string();
    parse(&edited)?;
    Ok(Some(edited))
}

/// Remove the entries of `finding` from `document`, saying whether there
/// were any. The entries are tables or an inline array of them, and comments
/// above a removed table stay.
fn without_finding(document: &mut DocumentMut, finding: FindingId) -> Result<bool> {
    match document.get_mut("false_positive") {
        None => Ok(false),
        Some(Item::ArrayOfTables(entries)) => {
            let (removed, orphaned) = without_table(entries, finding);
            if !orphaned.is_empty() {
                let trailing = orphaned + document.trailing().as_str().unwrap_or_default();
                document.set_trailing(trailing);
            }
            Ok(removed)
        }
        Some(Item::Value(Value::Array(entries))) => {
            let listed = entries.len();
            entries.retain(|entry| {
                let id = entry
                    .as_inline_table()
                    .and_then(|table| table.get("finding"));
                !is_of(id.and_then(Value::as_str), finding)
            });
            Ok(entries.len() < listed)
        }
        Some(_) => bail!("`false_positive` is not a list of tables"),
    }
}

/// Remove the tables of `finding` from `entries`, saying whether there were
/// any, and the comments above the last tables removed, which the next
/// table keeps otherwise.
fn without_table(entries: &mut ArrayOfTables, finding: FindingId) -> (bool, String) {
    let (mut index, mut removed, mut orphaned) = (0, false, String::new());
    while let Some(table) = entries.get(index) {
        if !is_of(table.get("finding").and_then(Item::as_str), finding) {
            index += 1;
            continue;
        }
        let comments = prefix(table.decor()).to_owned();
        entries.remove(index);
        removed = true;
        if !comments.contains('#') {
            continue;
        }
        match entries.get_mut(index) {
            Some(next) => {
                let kept = comments + prefix(next.decor());
                next.decor_mut().set_prefix(kept);
            }
            None => orphaned.push_str(&comments),
        }
    }
    (removed, orphaned)
}

/// Whether an entry's `finding` field names `finding`.
fn is_of(id: Option<&str>, finding: FindingId) -> bool {
    id.and_then(|id| id.parse::<FindingId>().ok()) == Some(finding)
}

fn prefix(decor: &toml_edit::Decor) -> &str {
    decor
        .prefix()
        .and_then(|prefix| prefix.as_str())
        .unwrap_or_default()
}

/// Add `entry` after the entries of `document`, in the form they take.
fn append(document: &mut DocumentMut, entry: &FalsePositive) {
    let mut table = Table::new();
    if let Some(finding) = entry.finding {
        table.insert("finding", value(finding.to_string()));
    }
    table.insert("claim", value(&entry.claim));
    table.insert("path", value(&entry.path));
    table.insert("reason", value(&entry.reason));
    match document.get_mut("false_positive") {
        Some(Item::ArrayOfTables(entries)) => {
            table.decor_mut().set_prefix("\n");
            entries.push(table);
        }
        Some(Item::Value(Value::Array(entries))) => entries.push(table.into_inline_table()),
        _ => {
            // Comments alone trail the document; keep them above the entry.
            let mut comments = document.trailing().as_str().unwrap_or_default().to_owned();
            if !comments.trim().is_empty() {
                comments.push('\n');
            }
            document.set_trailing("");
            table.decor_mut().set_prefix(comments);
            document.insert(
                "false_positive",
                Item::ArrayOfTables(ArrayOfTables::from_iter([table])),
            );
        }
    }
}

fn parse(text: &str) -> Result<Vec<FalsePositive>> {
    Ok(toml::from_str::<Entries>(text)?.false_positive)
}

/// The file's text, none without the file.
fn load(path: &Path) -> Result<Option<String>> {
    match std::fs::read_to_string(path) {
        Ok(text) => Ok(Some(text)),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error).with_context(|| format!("read {}", path.display())),
    }
}

/// Replace the file at `path` with `text`, creating its directory. A failed
/// write leaves the file as it was.
fn store(path: &Path, text: &str, exists: bool) -> Result<()> {
    if let Some(directory) = path.parent() {
        std::fs::create_dir_all(directory)
            .with_context(|| format!("create {}", directory.display()))?;
    }
    let permissions = match exists {
        true => Permissions::Preserve,
        false => Permissions::Mode(0o644),
    };
    let options = ReplaceOptions {
        permissions,
        sync: true,
    };
    fsutil::replace_atomically(path, text.as_bytes(), options)
        .with_context(|| format!("write {}", path.display()))
}

#[cfg(test)]
mod tests;
