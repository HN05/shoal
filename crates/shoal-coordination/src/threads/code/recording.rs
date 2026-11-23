//! Making a code reference from a path in a worktree: the space whose
//! worktree it is in, which the caller's scope must reach, the version of
//! its file, the snippet of its lines and HEAD.
use crate::{Coordination, daemon::Daemon};
use std::path::{Path, PathBuf};

use anyhow::{Context as _, Result, ensure};
use shoal_ids::{repository_ids::RepositoryId, space_ids::SpaceId};
use shoal_protocol::{
    code::{self, CodeAt, Lines, NewCodeRef},
    space_names::SpaceName,
};
use shoal_refs::CommitId;

use crate::{git, model::Workspace, scope::Caller};

/// A reference to record: the space whose worktree it is in, with its name
/// and repository, HEAD then, where it points and the snippet of its lines.
#[derive(Debug, Clone)]
pub(in crate::threads) struct NewCode {
    pub(in crate::threads) space_id: SpaceId,
    pub(in crate::threads) space_name: SpaceName,
    pub(in crate::threads) repository_id: RepositoryId,
    pub(in crate::threads) commit: Option<CommitId>,
    pub(in crate::threads) at: CodeAt,
    pub(in crate::threads) snippet: Option<String>,
}

/// The version of a file a reference keeps, and the snippet of its lines.
struct Version {
    blob: Option<String>,
    snippet: Option<String>,
}

impl Coordination {
    /// The references `code` names for `caller`, or the user without one,
    /// each in the worktree of a space its scope reaches, with the version
    /// of its file written into the repository.
    pub(in crate::threads) async fn new_code(
        &self,
        daemon: &dyn Daemon,
        caller: Option<&Caller>,
        code: Vec<NewCodeRef>,
    ) -> Result<Vec<NewCode>> {
        if code.is_empty() {
            return Ok(Vec::new());
        }
        let workspaces = daemon.list_workspaces().await?;
        let mut made = Vec::new();
        for new in code {
            made.push(self.code_ref(daemon, caller, &workspaces, new).await?);
        }
        Ok(made)
    }

    /// Whether `caller` points at code in the worktree of the workspace
    /// `id`: one its scope reaches, or the one its chat runs in, which a
    /// reference reads and changes nothing of.
    async fn references_into(
        &self,
        daemon: &dyn Daemon,
        caller: Option<&Caller>,
        id: &SpaceId,
    ) -> Result<bool> {
        let Some(caller) = caller else {
            return Ok(true);
        };
        if self.reaches(daemon, caller, id).await? {
            return Ok(true);
        }
        Ok(daemon.chat_home(&caller.space_id).await?.as_ref() == Some(id))
    }

    /// The reference `new` names among `workspaces` for `caller`.
    async fn code_ref(
        &self,
        daemon: &dyn Daemon,
        caller: Option<&Caller>,
        workspaces: &[Workspace],
        new: NewCodeRef,
    ) -> Result<NewCode> {
        let path = canonical(&new.path);
        let workspace = Workspace::innermost(workspaces, &path)
            .with_context(|| format!("{} is in no Shoal workspace", new.path.display()))?;
        ensure!(
            self.references_into(daemon, caller, &workspace.id).await?,
            "{} is in workspace {}, which your scope does not reach",
            new.path.display(),
            workspace.name
        );
        let root = canonical(&workspace.path);
        let relative = relative(&root, &path)?;
        let version = version(&root, &path, new.lines, new.contents).await?;
        let commit = (git::head_and_branch(&root).await)
            .ok()
            .map(|(commit, _)| commit);
        Ok(NewCode {
            space_id: workspace.id.clone(),
            space_name: workspace.name.clone(),
            repository_id: (workspace.repository_id.clone())
                .context("a workspace belongs to a repository")?,
            commit,
            at: CodeAt {
                path: relative,
                lines: new.lines,
                blob: version.blob,
            },
            snippet: version.snippet,
        })
    }
}

/// `path` with symlinks resolved, as workspace records hold their paths;
/// a file not yet on disk, such as an editor's new buffer, through its
/// directory's.
pub fn canonical(path: &Path) -> PathBuf {
    if let Ok(path) = std::fs::canonicalize(path) {
        return path;
    }
    match (path.parent(), path.file_name()) {
        (Some(parent), Some(name)) => {
            std::fs::canonicalize(parent).map_or_else(|_| path.to_owned(), |dir| dir.join(name))
        }
        _ => path.to_owned(),
    }
}

/// `path` relative to the worktree `root`, as Git names it.
pub fn relative(root: &Path, path: &Path) -> Result<String> {
    let relative = (path.strip_prefix(root))
        .with_context(|| format!("{} is outside {}", path.display(), root.display()))?;
    let parts: Option<Vec<&str>> = relative.iter().map(|part| part.to_str()).collect();
    let relative = parts
        .with_context(|| format!("{} is no UTF-8 path", path.display()))?
        .join("/");
    Ok(if relative.is_empty() {
        ".".into()
    } else {
        relative
    })
}

/// The version of the file at `path` in the worktree `root`, or `contents`
/// for it, that a reference to `lines` keeps; none for a directory, a
/// binary file or a large one.
async fn version(
    root: &Path,
    path: &Path,
    lines: Option<Lines>,
    contents: Option<String>,
) -> Result<Version> {
    let none = Version {
        blob: None,
        snippet: None,
    };
    if contents.is_none() && path.is_dir() {
        ensure!(
            lines.is_none(),
            "{} is a directory; point at it without lines",
            path.display()
        );
        return Ok(none);
    }
    let bytes = file_contents(path, contents).await?;
    if let Some(lines) = lines {
        ensure_has(path, &bytes, lines)?;
    }
    let Some(text) = super::kept_text(bytes) else {
        return Ok(none);
    };
    let snippet = lines.map(|lines| code::snippet(&text, lines));
    let blob = git::code_versions::write(root, text).await?;
    Ok(Version {
        blob: Some(blob),
        snippet,
    })
}

/// `contents`, an editor's buffer, else the file at `path` on disk.
async fn file_contents(path: &Path, contents: Option<String>) -> Result<Vec<u8>> {
    if let Some(contents) = contents {
        return Ok(contents.into_bytes());
    }
    let file = path.to_owned();
    (shoal_core::fsutil::off_runtime::run(move || Ok(std::fs::read(file)?)).await)
        .with_context(|| format!("read {}", path.display()))
}

/// Refuse `lines` that end before they start, or past the end of the file
/// at `path`, which holds `bytes`.
fn ensure_has(path: &Path, bytes: &[u8], lines: Lines) -> Result<()> {
    ensure!(
        lines.start < lines.end,
        "lines {}-{} end before they start",
        lines.start + 1,
        lines.end
    );
    let count = line_count(bytes);
    ensure!(
        lines.end <= count,
        "{} has {count} lines, so it has no lines {lines}",
        path.display()
    );
    Ok(())
}

/// How many lines `bytes` has, a last one without a newline included.
fn line_count(bytes: &[u8]) -> u32 {
    let newlines = bytes.iter().filter(|byte| **byte == b'\n').count();
    let unterminated = !bytes.is_empty() && !bytes.ends_with(b"\n");
    u32::try_from(newlines + usize::from(unterminated)).unwrap_or(u32::MAX)
}
