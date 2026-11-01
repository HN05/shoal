//! Filesystem reads on Tokio's blocking pool. A read on a volume whose file
//! access stalls waits in the kernel, where no time limit can interrupt it;
//! on the blocking pool it holds no runtime thread, so other tasks keep
//! running and a caller's time limit still ends the wait for it.
use std::path::{Path, PathBuf};

use anyhow::Result;

/// Run the blocking file access `read` on the blocking pool.
pub async fn run<T: Send + 'static>(
    read: impl FnOnce() -> Result<T> + Send + 'static,
) -> Result<T> {
    tokio::task::spawn_blocking(read).await?
}

/// Whether `path` exists, read as [`Path::exists`] does.
pub async fn exists(path: &Path) -> Result<bool> {
    let path = path.to_owned();
    run(move || Ok(path.exists())).await
}

/// Whether `path` exists, read as [`Path::try_exists`] does.
pub async fn try_exists(path: &Path) -> Result<bool> {
    let path = path.to_owned();
    run(move || Ok(path.try_exists()?)).await
}

/// Whether `path` is a directory, read as [`Path::is_dir`] does.
pub async fn is_dir(path: &Path) -> Result<bool> {
    let path = path.to_owned();
    run(move || Ok(path.is_dir())).await
}

/// The canonical form of `path`, as [`std::fs::canonicalize`] gives it.
pub async fn canonicalize(path: &Path) -> Result<PathBuf> {
    let path = path.to_owned();
    run(move || Ok(std::fs::canonicalize(path)?)).await
}
