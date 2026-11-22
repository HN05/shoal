//! Files attached to a workspace or chat: Shoal's copies and how they came.
use crate::space_names::SpaceName;
use shoal_ids::execution_ids::ExecutionId;
use shoal_ids::space_ids::SpaceId;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, ensure};
use serde::{Deserialize, Serialize};
use shoal_states::states;

states!(FileSource {
    /// Sent with a message to the workspace's agents.
    Message => "message",
    /// Added with `shoal files add`.
    Added => "added",
    /// Handed back with `shoal done`.
    Result => "result",
    /// Sent up with `shoal send`.
    Alert => "alert",
    /// Sent with a post in a thread.
    Thread => "thread",
});

/// Who attached a file.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Attacher {
    /// A request from outside any agent.
    User,
    /// A request from an agent's process tree, a workspace's or a chat's.
    Agent {
        /// The agent, named as `--agent` names it.
        agent: crate::agent::Agent,
        /// The execution that ran the agent, which a restart replaces.
        execution: ExecutionId,
        /// The session the agent reported, which outlives a resume; none
        /// before its first report.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        session: Option<shoal_ids::agent_session_ids::AgentSessionId>,
    },
}

/// The largest file a message may attach.
pub const MAX_FILE_BYTES: u64 = 100 * 1024 * 1024;

/// Refuse what a message cannot attach: a relative path, something other
/// than a file, or a file over [`MAX_FILE_BYTES`].
pub fn check_path(path: &Path) -> Result<()> {
    let shown = path.display();
    ensure!(path.is_absolute(), "{shown}: give an absolute path");
    let metadata = std::fs::metadata(path).with_context(|| format!("read {shown}"))?;
    ensure!(metadata.is_file(), "{shown} is not a file");
    ensure!(
        metadata.len() <= MAX_FILE_BYTES,
        "{shown}: attached files must be at most {} MiB",
        MAX_FILE_BYTES / 1024 / 1024
    );
    Ok(())
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

/// The most files one alert or post sends, which keeps one with the
/// paths of Shoal's copies to a small part of a protocol frame, so listings
/// of several still fit one response.
pub const MAX_SENT: usize = 20;

/// Refuse more files than one alert or post sends.
pub fn check_sent(files: &[NewFile]) -> Result<()> {
    ensure!(
        files.len() <= MAX_SENT,
        "send at most {MAX_SENT} files with one alert or post"
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

/// Where Shoal's copy of an attached file is: at a path on the machine
/// that answered, or on another joined machine, which alone keeps it and
/// from which a client reads it by its space and record ID.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum KeptCopy {
    Here { path: PathBuf },
    Elsewhere { machine: String },
}

impl KeptCopy {
    /// The copy's path, on the machine that keeps it.
    pub fn path(&self) -> Option<&Path> {
        match self {
            Self::Here { path } => Some(path),
            Self::Elsewhere { .. } => None,
        }
    }

    /// Where it is, as a listing shows it: its path, or the machine that
    /// keeps it.
    pub fn label(&self) -> String {
        match self {
            Self::Here { path } => path.display().to_string(),
            Self::Elsewhere { machine } => format!("on machine {machine}"),
        }
    }

    /// The copy as a machine other than `machine`, which keeps it, sees
    /// it: by that machine, since the path names a file there.
    pub fn kept_on(&mut self, machine: &str) {
        if let Self::Here { .. } = self {
            *self = Self::Elsewhere {
                machine: machine.to_owned(),
            };
        }
    }
}

/// A file an agent sent with an alert or a post: the record of
/// Shoal's copy, which `shoal files` lists, and where the copy is.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SentFile {
    pub id: i64,
    /// The space it is attached to, which with `id` selects it.
    pub space: SpaceId,
    /// The attached file's name.
    pub name: String,
    /// What the file is, when the agent said.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    #[serde(flatten)]
    pub copy: KeptCopy,
}

impl From<&AttachedFile> for SentFile {
    fn from(file: &AttachedFile) -> Self {
        Self {
            id: file.id,
            space: file.space.id.clone(),
            name: file.name.clone(),
            title: file.title.clone(),
            copy: file.copy.clone(),
        }
    }
}

impl SentFile {
    /// Its copy's path, on the machine that keeps it.
    pub fn path(&self) -> Option<&Path> {
        self.copy.path()
    }

    /// Where it is, as a line that lists it shows: its copy's path, or its
    /// name and the machine that keeps it.
    pub fn shown(&self) -> String {
        match &self.copy {
            KeptCopy::Here { path } => path.display().to_string(),
            elsewhere => format!("{} {}", self.name, elsewhere.label()),
        }
    }
}

/// The paths of the copies of `files` that the machine reading them keeps.
pub fn paths_here(files: &[SentFile]) -> Vec<PathBuf> {
    (files.iter())
        .filter_map(|file| file.copy.path().map(Path::to_path_buf))
        .collect()
}

/// The space a file is attached to, which may be gone.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AttachedTo {
    pub id: SpaceId,
    /// Its name when the file was attached, or its last once it is removed.
    pub name: SpaceName,
    pub kind: crate::model::SpaceKind,
}

/// A file attached to a workspace or chat: Shoal's copy and how it came.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AttachedFile {
    pub id: i64,
    /// The space it is attached to.
    pub space: AttachedTo,
    pub source: FileSource,
    /// The attached file's name.
    pub name: String,
    /// What the file is, when whoever attached it said; clients show the
    /// name otherwise.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    #[serde(flatten)]
    pub copy: KeptCopy,
    pub bytes: u64,
    pub created_at: crate::time::UnixTime,
    pub attached_by: Attacher,
    /// When the file of a removed space is deleted.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expires_at: Option<crate::time::UnixTime>,
}

impl AttachedFile {
    /// Its copy's path, on the machine that keeps it.
    pub fn path(&self) -> Option<&Path> {
        self.copy.path()
    }
}
