//! Files attached to a workspace or chat: Shoal's copies and how they came.
use std::path::PathBuf;

use anyhow::{Result, ensure};
use serde::{Deserialize, Serialize};

use crate::state::states;

states!(FileSource {
    /// Sent with a message to the workspace's agents.
    Message => "message",
    /// Added with `shoal files add`.
    Added => "added",
    /// Handed back with `shoal done`; kept after the workspace's removal for
    /// as long as its removal record.
    Result => "result",
    /// Sent to the user with `shoal alerts send`; kept as results are.
    Alert => "alert",
    /// Sent to the user with `shoal questions ask`; kept as results are.
    Question => "question",
});

impl FileSource {
    /// The sources whose files a workspace hands the user, which stay after
    /// its removal for as long as its removal record.
    pub const KEPT: [Self; 3] = [Self::Result, Self::Alert, Self::Question];
}

/// Who attached a file.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Attacher {
    /// A request from outside any agent.
    User,
    /// A request from an agent's process tree.
    Agent {
        /// The agent, named as `--agent` names it.
        agent: String,
        /// The execution that ran the agent, which a restart replaces.
        execution: String,
        /// The session the agent reported, which outlives a resume; none
        /// before its first report.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        session: Option<String>,
    },
    /// A request from a chat's agent, which runs outside any execution.
    Chat {
        /// The chat's agent, named as `--agent` names it.
        agent: String,
        /// The chat's ID.
        chat: String,
    },
}

/// The most files one completion hands back, which keeps the completion that
/// lists their copies to one protocol frame.
pub const MAX_RESULTS: usize = 64;

/// Refuse more results than one completion hands back.
pub fn check_results(results: &[NewFile]) -> Result<()> {
    ensure!(
        results.len() <= MAX_RESULTS,
        "hand back at most {MAX_RESULTS} files with one completion"
    );
    Ok(())
}

/// The `names` and record `ids` that none of `files` has, as an error
/// names them.
pub fn missing_selection(files: &[AttachedFile], names: &[String], ids: &[i64]) -> Vec<String> {
    names
        .iter()
        .filter(|name| !files.iter().any(|file| file.name == **name))
        .cloned()
        .chain(
            ids.iter()
                .filter(|id| !files.iter().any(|file| file.id == **id))
                .map(|id| format!("number {id}")),
        )
        .collect()
}

/// A file to attach, by absolute path, with the title that says what it is.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NewFile {
    pub path: PathBuf,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
}

impl From<PathBuf> for NewFile {
    fn from(path: PathBuf) -> Self {
        Self { path, title: None }
    }
}

impl NewFile {
    /// Refuse a title that is not a single line of text.
    pub fn check(&self) -> Result<()> {
        self.title
            .as_deref()
            .map_or(Ok(()), crate::validate::file_title)
    }
}

/// A file an agent sent the user with an alert or a question: the record of
/// Shoal's copy, which `shoal files` lists, and where the copy is.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SentFile {
    pub id: i64,
    /// The attached file's name.
    pub name: String,
    /// What the file is, when the agent said.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    /// Shoal's copy.
    pub path: PathBuf,
}

impl From<&AttachedFile> for SentFile {
    fn from(file: &AttachedFile) -> Self {
        Self {
            id: file.id,
            name: file.name.clone(),
            title: file.title.clone(),
            path: file.path.clone(),
        }
    }
}

/// A file attached to a workspace or chat: Shoal's copy and how it came.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AttachedFile {
    pub id: i64,
    /// The name of the workspace it is attached to: when it was attached, or
    /// the last one once the workspace is removed. None for a chat's file.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workspace: Option<String>,
    /// The name of the chat it is attached to, as [`Self::workspace`] names
    /// a workspace's.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub chat: Option<String>,
    pub source: FileSource,
    /// The attached file's name.
    pub name: String,
    /// What the file is, when whoever attached it said; clients show the
    /// name otherwise.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    /// Shoal's copy.
    pub path: PathBuf,
    pub bytes: u64,
    /// Unix seconds.
    pub created_at: i64,
    /// None for a file attached before Shoal recorded who attached it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub attached_by: Option<Attacher>,
    /// When the file of a removed workspace or ended chat is deleted, in
    /// Unix seconds.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expires_at: Option<i64>,
}
