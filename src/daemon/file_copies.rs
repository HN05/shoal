//! Copies of attached files the daemon keeps, so the files stay as they
//! were when attached.
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, ensure};

use crate::forge::items::attach::Attachment;

/// Copies [`keep_copies`] made or reused.
pub(crate) struct Copies {
    pub(crate) attachments: Vec<Attachment>,
    /// The directory holding them when this call made it, so a caller that
    /// cannot use them may delete it; copies reused from earlier stay.
    pub(crate) created: Option<PathBuf>,
}

/// The copies of `attachments` below `root`, in a directory named by the
/// owner, text and files that attach them. An existing directory is reused
/// unchanged; a new one is filled under a temporary name first, so a partial
/// copy is never reused.
pub(crate) fn keep_copies(
    root: &Path,
    owner: &str,
    body: &str,
    attachments: &[Attachment],
) -> Result<Copies> {
    let directory = root.join(content_key(owner, body, attachments)?);
    let mut created = None;
    if !directory.exists() {
        let staging = root.join(format!(".copying-{}", uuid::Uuid::new_v4()));
        let copied = copy_attachments(&staging, attachments)
            .and_then(|()| std::fs::rename(&staging, &directory).map_err(Into::into));
        if let Err(error) = copied {
            std::fs::remove_dir_all(&staging).ok();
            ensure!(
                directory.exists(),
                "keep {}: {error:#}",
                directory.display()
            );
        } else {
            created = Some(directory.clone());
        }
    }
    Ok(Copies {
        attachments: attachments
            .iter()
            .enumerate()
            .map(|(index, attachment)| Attachment {
                path: copy_path(&directory, index, attachment),
                alt: attachment.alt.clone(),
            })
            .collect(),
        created,
    })
}

/// A name for the files attached with this owner, text and attachments,
/// their contents included.
fn content_key(owner: &str, body: &str, attachments: &[Attachment]) -> Result<String> {
    use sha2::{Digest, Sha256};
    let mut hasher = Sha256::new();
    for part in [owner, body] {
        hasher.update(part.as_bytes());
        hasher.update([0]);
    }
    for attachment in attachments {
        hasher.update(attachment.path.as_os_str().as_encoded_bytes());
        hasher.update([0]);
        hasher.update(attachment.alt.as_deref().unwrap_or_default().as_bytes());
        hasher.update([0]);
        let contents = std::fs::read(&attachment.path)
            .with_context(|| format!("read {}", attachment.path.display()))?;
        hasher.update(Sha256::digest(&contents));
    }
    Ok(hasher.finalize()[..16]
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect())
}

/// Where the attachment at `index` is copied below `directory`: its own
/// directory, keeping the file name, which names the file where it is shown.
fn copy_path(directory: &Path, index: usize, attachment: &Attachment) -> PathBuf {
    let name = attachment.path.file_name().unwrap_or_default();
    directory.join(index.to_string()).join(name)
}

/// Copy each attachment to its [`copy_path`] below `directory`.
fn copy_attachments(directory: &Path, attachments: &[Attachment]) -> Result<()> {
    for (index, attachment) in attachments.iter().enumerate() {
        let target = copy_path(directory, index, attachment);
        if let Some(parent) = target.parent() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("create {}", parent.display()))?;
        }
        std::fs::copy(&attachment.path, &target)
            .with_context(|| format!("copy {}", attachment.path.display()))?;
    }
    Ok(())
}
