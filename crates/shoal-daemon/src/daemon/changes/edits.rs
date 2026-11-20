//! An edit tool's change: the hunks its hook passes are the agent's, of
//! the file version on disk when the hook runs; a hunk whose lines the
//! file no longer holds where it put them is mixed, since someone else
//! edited the file too. What changed in the file since the agent's last
//! tool call, the file before the edit against the snapshot base, is
//! someone else's, or the agent's while one of its commands runs. The
//! version then goes into the base, so a later diff takes neither for a
//! new change.
use std::path::Path;

use anyhow::Result;
use shoal_protocol::{
    changes::{Change, ChangeAuthor, ChangeKind, Hunk, diff, unapply},
    code::MAX_VERSIONED_LEN,
};

use super::records::{self, NewChange};
use crate::{
    daemon::{manager::Manager, threads},
    git,
    model::Workspace,
};

/// The version of a file an edit tool wrote, as it is on disk now.
struct Version {
    /// Its path, relative to the worktree's root.
    path: String,
    kind: ChangeKind,
    text: String,
    size: u64,
    /// The blob it is, written into the repository.
    blob: String,
}

impl Manager {
    /// `workspace`'s agent's edit tool changed the file at `path` as
    /// `hunks` say; nothing for a file outside its worktree.
    pub(super) async fn record_edit(
        &self,
        workspace: &Workspace,
        path: &Path,
        hunks: Vec<Hunk>,
    ) -> Result<Vec<Change>> {
        let Some(version) = Version::on_disk(workspace, path).await? else {
            return Ok(Vec::new());
        };
        let base = self.change_base(workspace).await?;
        let mut changes = match &base {
            Some(base) => {
                let author = self.author_between_calls(workspace).await?;
                changed_before(workspace, base, &version, &hunks, author).await?
            }
            None => Vec::new(),
        };
        changes.extend(version.changes(hunks));
        let base = match base {
            Some(base) => Some(with_version(workspace, &base, &version).await?),
            None => None,
        };
        let made = self.made_in(workspace).await?;
        let id = workspace.id.clone();
        (self.store)
            .write(move |tx| {
                let recorded = records::insert(tx, &made, &changes)?;
                if let Some(base) = base {
                    records::set_base(tx, &id, &base)?;
                }
                Ok(recorded)
            })
            .await
    }
}

/// The snapshot base `base` of `workspace` with `version` in it.
async fn with_version(workspace: &Workspace, base: &str, version: &Version) -> Result<String> {
    git::changes::with_version(&workspace.path, base, &version.path, Some(&version.blob)).await
}

/// The changes `author` made to the text file of `version` since the base
/// `base` of `workspace`: the base's version of it against the file before
/// `hunks`, which the edit tool then made; none when the file no longer
/// holds the hunks where they say, which leaves them mixed instead.
async fn changed_before(
    workspace: &Workspace,
    base: &str,
    version: &Version,
    hunks: &[Hunk],
    author: ChangeAuthor,
) -> Result<Vec<NewChange>> {
    let Some(before) = unapply(&version.text, hunks).filter(|_| version.kind == ChangeKind::Text)
    else {
        return Ok(Vec::new());
    };
    let kept = match git::changes::blob_in(&workspace.path, base, &version.path).await? {
        Some(blob) => git::code_versions::read(&workspace.path, &blob).await.ok(),
        None => Some(String::new()),
    };
    let Some(kept) = kept.filter(|kept| *kept != before) else {
        return Ok(Vec::new());
    };
    let blob = git::code_versions::write(&workspace.path, before.clone()).await?;
    Ok((diff(&kept, &before).into_iter())
        .map(|hunk| NewChange {
            author,
            kind: ChangeKind::Text,
            path: version.path.clone(),
            hunk: Some(hunk),
            blob: Some(blob.clone()),
            size: None,
        })
        .collect())
}

impl Version {
    /// The file at `path` in `workspace`'s worktree as it is now; none
    /// outside the worktree, for a file Git ignores, as snapshots do, or
    /// once it is gone.
    async fn on_disk(workspace: &Workspace, path: &Path) -> Result<Option<Self>> {
        let root = threads::canonical(&workspace.path);
        let Ok(relative) = threads::relative(&root, &threads::canonical(path)) else {
            return Ok(None);
        };
        if git::changes::ignored(&root, &relative).await? {
            return Ok(None);
        }
        let file = root.join(&relative);
        let read = crate::fsutil::off_runtime::run(move || Ok(std::fs::read(file).ok()));
        let Some(bytes) = read.await? else {
            return Ok(None);
        };
        let generated = git::changes::generated(&root, &[&relative]).await?;
        let kind = kind(&bytes, generated.contains(&relative));
        Ok(Some(Self {
            kind,
            size: bytes.len() as u64,
            text: String::from_utf8_lossy(&bytes).into_owned(),
            blob: git::code_versions::write(&root, bytes).await?,
            path: relative,
        }))
    }

    /// The changes `hunks` made to it: one per hunk of a text file, mixed
    /// where it no longer holds the hunk's lines, or one without hunks.
    fn changes(&self, hunks: Vec<Hunk>) -> Vec<NewChange> {
        if self.kind != ChangeKind::Text {
            return vec![NewChange {
                author: ChangeAuthor::Agent,
                kind: self.kind,
                path: self.path.clone(),
                hunk: None,
                blob: None,
                size: Some(self.size),
            }];
        }
        (hunks.into_iter())
            .map(|hunk| NewChange {
                author: match holds(&self.text, &hunk) {
                    true => ChangeAuthor::Agent,
                    false => ChangeAuthor::Mixed,
                },
                kind: self.kind,
                path: self.path.clone(),
                hunk: Some(hunk),
                blob: Some(self.blob.clone()),
                size: None,
            })
            .collect()
    }
}

/// What a file holding `bytes` is to a change.
fn kind(bytes: &[u8], generated: bool) -> ChangeKind {
    if generated {
        ChangeKind::Generated
    } else if bytes.len() > MAX_VERSIONED_LEN {
        ChangeKind::Large
    } else if std::str::from_utf8(bytes).is_err() || bytes.contains(&0) {
        ChangeKind::Binary
    } else {
        ChangeKind::Text
    }
}

/// Whether `text` holds the lines `hunk` added where it says it put them.
fn holds(text: &str, hunk: &Hunk) -> bool {
    let lines = text.lines().skip(hunk.lines.start as usize);
    let added = hunk.added.iter().map(|line| line.trim_end_matches('\r'));
    lines.take(hunk.added.len()).eq(added)
}
