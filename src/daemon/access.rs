//! Whether the daemon can write a directory before it acts on one. The daemon
//! can lack file access the user's terminal has: on macOS, opening a file on a
//! volume the daemon has no privacy access to can wait for a permission no one
//! is shown, so the check gives up after a time limit.
use std::{io, path::Path, time::Duration};

use anyhow::{Context, Result, anyhow};
use tokio::sync::oneshot;

/// How long creating a file may take, generous enough for a disk to wake.
const LIMIT: Duration = Duration::from_secs(30);

/// Refuse unless the daemon can create a file in `directory`, or, when it is
/// missing, in the nearest directory above it, where creating it would write.
pub async fn ensure_writable(directory: &Path) -> Result<()> {
    within_limit(directory, LIMIT, create_file).await
}

/// Run `operation` on `directory` on a thread of its own, which is left
/// behind when it takes longer than `limit`: a blocked `open` cannot be
/// interrupted, and a detached thread keeps neither the request nor the
/// daemon's shutdown waiting.
async fn within_limit(
    directory: &Path,
    limit: Duration,
    operation: impl FnOnce(&Path) -> io::Result<()> + Send + 'static,
) -> Result<()> {
    let (sender, receiver) = oneshot::channel();
    let target = directory.to_owned();
    std::thread::Builder::new()
        .name("shoal-access-check".to_owned())
        .spawn(move || {
            let _ = sender.send(operation(&target));
        })
        .context("start the file access check")?;
    let cannot_write = || format!("the Shoal daemon cannot write to {}", directory.display());
    match tokio::time::timeout(limit, receiver).await {
        Ok(Ok(result)) => result.with_context(cannot_write),
        Ok(Err(_)) => Err(anyhow!("the file access check stopped")).with_context(cannot_write),
        Err(_) => Err(anyhow!(
            "creating a file there did not finish within {} seconds; macOS may not have given the daemon access to that volume",
            limit.as_secs()
        ))
        .with_context(cannot_write),
    }
}

/// Create and delete a file in `directory` or its nearest existing ancestor.
fn create_file(directory: &Path) -> io::Result<()> {
    let existing = directory
        .ancestors()
        .find(|path| path.exists())
        .unwrap_or(directory);
    let file = existing.join(format!(".shoal-access-{}", uuid::Uuid::new_v4()));
    std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&file)?;
    std::fs::remove_file(file)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn a_writable_directory_or_its_existing_ancestor_passes_and_keeps_no_file() {
        let root = tempfile::tempdir().unwrap();
        ensure_writable(root.path()).await.unwrap();
        ensure_writable(&root.path().join("missing/below"))
            .await
            .unwrap();
        assert_eq!(std::fs::read_dir(root.path()).unwrap().count(), 0);
    }

    #[tokio::test]
    async fn a_failed_file_names_the_directory() {
        let root = tempfile::tempdir().unwrap();
        let file = root.path().join("file");
        std::fs::write(&file, "").unwrap();
        let directory = file.join("exports");
        let error = ensure_writable(&directory).await.unwrap_err();
        assert!(
            format!("{error:#}").starts_with(&format!(
                "the Shoal daemon cannot write to {}: ",
                directory.display()
            )),
            "{error:#}"
        );
    }

    #[tokio::test]
    async fn a_file_that_cannot_be_created_in_time_fails_at_the_limit() {
        let (release, blocked) = std::sync::mpsc::channel::<()>();
        let error = within_limit(Path::new("/volume"), Duration::from_secs(2), move |_| {
            let _ = blocked.recv();
            Ok(())
        })
        .await
        .unwrap_err();
        drop(release);
        assert_eq!(
            format!("{error:#}"),
            "the Shoal daemon cannot write to /volume: creating a file there did not finish within 2 seconds; macOS may not have given the daemon access to that volume"
        );
    }
}
